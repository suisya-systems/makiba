//! M1-5: what makiba uses of the base (`src/base/uses.rs`) is checked against the base's own
//! definition: herdr-core's OpenRPC schema. The definition is, in order: the file named by
//! `MAKIBA_BASE_SCHEMA` (a candidate release's `hc schema`, before the pin moves), `hc schema` of a
//! base started in a place of its own (`MAKIBA_TEST_REAL_BASE=1`), or the vendored copy of the
//! pinned release, `docs/base/hc.openrpc.json`.

use makiba::base::uses::{Field, USES, Use};
use makiba::base::version::PINNED;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

// ---- reading a definition -------------------------------------------------------------------

type Res<T> = Result<T, String>;

/// Follow `$ref` (to a place inside the same document) until a schema is reached.
fn deref<'a>(def: &'a Value, mut s: &'a Value) -> Res<&'a Value> {
    for _ in 0..32 {
        let Some(r) = s.get("$ref").and_then(Value::as_str) else {
            return Ok(s);
        };
        let at = r
            .strip_prefix('#')
            .ok_or_else(|| format!("unsupported $ref {r}"))?;
        s = def
            .pointer(at)
            .ok_or_else(|| format!("unresolved $ref {r}"))?;
    }
    Err("$ref loop".into())
}

/// The schema and every schema it is made of (`allOf`, `oneOf`, `anyOf`), with `$ref` followed.
fn parts<'a>(def: &'a Value, s: &'a Value, out: &mut Vec<&'a Value>, depth: u32) -> Res<()> {
    if depth > 32 {
        return Err("schema nests too deeply".into());
    }
    let s = deref(def, s)?;
    out.push(s);
    for key in ["allOf", "oneOf", "anyOf"] {
        for sub in s.get(key).and_then(Value::as_array).into_iter().flatten() {
            parts(def, sub, out, depth + 1)?;
        }
    }
    Ok(())
}

fn all_parts<'a>(def: &'a Value, s: &'a Value) -> Res<Vec<&'a Value>> {
    let mut out = Vec::new();
    parts(def, s, &mut out, 0)?;
    Ok(out)
}

fn types(def: &Value, s: &Value) -> Res<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    for p in all_parts(def, s)? {
        match p.get("type") {
            Some(Value::String(t)) => {
                found.insert(t.clone());
            }
            Some(Value::Array(ts)) => {
                found.extend(ts.iter().filter_map(Value::as_str).map(String::from))
            }
            _ => {}
        }
    }
    Ok(found)
}

/// The schema at a dotted path (`core.version`, `terminals[].labels`), or why there is none.
fn walk<'a>(def: &'a Value, root: &'a Value, path: &str) -> Res<&'a Value> {
    let mut at = root;
    for seg in path.split('.') {
        let (name, array) = match seg.strip_suffix("[]") {
            Some(n) => (n, true),
            None => (seg, false),
        };
        at = all_parts(def, at)?
            .into_iter()
            .find_map(|p| p.pointer(&format!("/properties/{name}")))
            .ok_or("no such field")?;
        if array {
            at = all_parts(def, at)?
                .into_iter()
                .find_map(|p| p.get("items"))
                .ok_or("not an array")?;
        }
    }
    Ok(at)
}

/// Every way `uses` does not hold against `def`, one line each. Each line names the method and the
/// field, so a failing run says what to look at.
fn problems(def: &Value, uses: &[Use]) -> Vec<String> {
    let mut out = Vec::new();
    let methods = def.get("methods").and_then(Value::as_array);
    for u in uses {
        let Some(m) = methods.and_then(|ms| {
            ms.iter()
                .find(|m| m.get("name").and_then(Value::as_str) == Some(u.method))
        }) else {
            out.push(format!("{}: the method is not in the definition", u.method));
            continue;
        };
        let declared = m.get("params").and_then(Value::as_array);
        let declared = declared.map(Vec::as_slice).unwrap_or_default();
        for p in u.params {
            if !declared
                .iter()
                .any(|d| d.get("name").and_then(Value::as_str) == Some(p))
            {
                out.push(format!("{}: param {p} is not in the definition", u.method));
            }
        }
        for d in declared {
            let name = d.get("name").and_then(Value::as_str).unwrap_or("?");
            if d.get("required") == Some(&json!(true)) && !u.params.contains(&name) {
                out.push(format!(
                    "{}: param {name} is required and makiba does not give it",
                    u.method
                ));
            }
        }
        let root = m.pointer("/result/schema");
        for f in u.result {
            let found = match root {
                Some(r) => walk(def, r, f.path),
                None => Err("the method has no result schema".into()),
            };
            match found {
                Err(why) => out.push(format!("{}: result field {} ({why})", u.method, f.path)),
                Ok(s) => match types(def, s) {
                    Err(why) => out.push(format!("{}: result field {} ({why})", u.method, f.path)),
                    Ok(t) if f.ty.is_empty() || t.is_empty() || t.contains(f.ty) => {}
                    Ok(t) => out.push(format!(
                        "{}: result field {} is {t:?}, makiba reads {}",
                        u.method, f.path, f.ty
                    )),
                },
            }
        }
    }
    out
}

// ---- changing a copy ------------------------------------------------------------------------

/// Remove property `name` wherever the document declares it, and from every `required` list.
fn strip_property(v: &mut Value, name: &str) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Object(props)) = m.get_mut("properties") {
                props.remove(name);
            }
            if let Some(Value::Array(req)) = m.get_mut("required") {
                req.retain(|r| r.as_str() != Some(name));
            }
            m.values_mut().for_each(|x| strip_property(x, name));
        }
        Value::Array(a) => a.iter_mut().for_each(|x| strip_property(x, name)),
        _ => {}
    }
}

fn methods_mut(def: &mut Value) -> &mut Vec<Value> {
    def["methods"]
        .as_array_mut()
        .expect("the definition has methods")
}

// ---- where the definition comes from ----------------------------------------------------------

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn vendored() -> Value {
    let path = root().join("docs/base/hc.openrpc.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the vendored definition {}: {e}", path.display()));
    serde_json::from_str(&text).expect("the vendored definition is JSON")
}

/// `hc schema` of a base in a place of its own, stopped afterwards. Honors `HC_BIN`.
fn from_real_base() -> Value {
    let hc = std::env::var_os("HC_BIN").unwrap_or_else(|| "hc".into());
    let root = std::env::temp_dir().join(format!("mk-compat-{}", std::process::id()));
    let (config, state, run) = (root.join("config"), root.join("state"), root.join("run"));
    for d in [&config, &state, &run] {
        fs::create_dir_all(d.join("herdr-core")).expect("make the place's folders");
    }
    fs::write(config.join("herdr-core/config.toml"), "notes = false\n").expect("write config");
    let hc_in_place = |args: &[&str]| {
        let mut c = Command::new(&hc);
        for v in [
            "HC_SOCKET",
            "HC_SCOPE",
            "HC_TERMINAL_ID",
            "HC_TERMINAL_SOCKET",
        ] {
            c.env_remove(v);
        }
        c.env("HC_SESSION", format!("makiba-test-{}", std::process::id()))
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_RUNTIME_DIR", &run)
            .args(args)
            .output()
            .expect("run hc")
    };
    let started = hc_in_place(&["server", "start"]);
    let out = hc_in_place(&["schema"]);
    hc_in_place(&["server", "stop"]);
    let _ = fs::remove_dir_all(&root);
    assert!(started.status.success(), "hc server start failed");
    assert!(out.status.success(), "hc schema failed");
    serde_json::from_slice(&out.stdout).expect("hc schema prints JSON")
}

/// The definition the compatibility check runs on.
fn definition() -> Value {
    if let Some(file) = std::env::var_os("MAKIBA_BASE_SCHEMA") {
        let text = fs::read_to_string(&file).expect("read MAKIBA_BASE_SCHEMA");
        return serde_json::from_str(&text).expect("MAKIBA_BASE_SCHEMA is JSON");
    }
    if std::env::var("MAKIBA_TEST_REAL_BASE").as_deref() == Ok("1") {
        return from_real_base();
    }
    vendored()
}

// ---- the tests --------------------------------------------------------------------------------

/// Green on the pinned version: nothing makiba uses is missing from the base's own definition.
#[test]
fn every_use_is_in_the_definition() {
    let found = problems(&definition(), USES);
    assert!(
        found.is_empty(),
        "makiba uses what the base lacks:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_vendored_definition_is_the_pinned_release() {
    let v = vendored();
    assert_eq!(
        v.pointer("/info/version").and_then(Value::as_str),
        Some(PINNED)
    );
    assert!(v.get("openrpc").is_some(), "not an OpenRPC document");
}

/// With one used field removed from a copy of the definition, the test names that field.
#[test]
fn a_removed_result_field_is_named() {
    let def = vendored();
    for u in USES {
        for f in u.result {
            let name = f.path.rsplit('.').next().unwrap().trim_end_matches("[]");
            let mut copy = def.clone();
            strip_property(&mut copy, name);
            let found = problems(&copy, USES);
            let want = format!("{}: result field {}", u.method, f.path);
            assert!(
                found.iter().any(|l| l.starts_with(&want)),
                "removing {name} must name {want}; got {found:?}"
            );
        }
    }
}

#[test]
fn a_removed_param_or_method_is_named() {
    let def = vendored();
    for u in USES {
        for p in u.params {
            let mut copy = def.clone();
            for m in methods_mut(&mut copy) {
                if m["name"] == u.method {
                    m["params"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|d| d["name"] != *p);
                }
            }
            let found = problems(&copy, USES);
            let want = format!("{}: param {p} is not in the definition", u.method);
            assert!(found.contains(&want), "want {want:?}; got {found:?}");
        }
        let mut copy = def.clone();
        methods_mut(&mut copy).retain(|m| m["name"] != u.method);
        let found = problems(&copy, USES);
        let want = format!("{}: the method is not in the definition", u.method);
        assert!(found.contains(&want), "want {want:?}; got {found:?}");
    }
}

#[test]
fn the_uses_are_listed_once_and_not_empty() {
    assert!(USES.iter().any(|u| u.method == "hello"));
    let methods: BTreeSet<_> = USES.iter().map(|u| u.method).collect();
    assert_eq!(methods.len(), USES.len(), "a method is listed twice");
    for u in USES {
        let paths: BTreeSet<_> = u.result.iter().map(|f| f.path).collect();
        assert_eq!(
            paths.len(),
            u.result.len(),
            "{}: a field is listed twice",
            u.method
        );
        let params: BTreeSet<_> = u.params.iter().collect();
        assert_eq!(
            params.len(),
            u.params.len(),
            "{}: a param is listed twice",
            u.method
        );
    }
}

/// `makiba down --stop-base` calls `server.stop` with no params and reads no field of its result
/// (`src/base/herdr.rs`), so the method is listed and a release without it, or one that makes it
/// take a param makiba does not give, is named (Issue 421).
#[test]
fn server_stop_is_listed_and_checked() {
    let stop = USES.iter().find(|u| u.method == "server.stop");
    let stop = stop.expect("server.stop is listed in src/base/uses.rs");
    assert!(stop.params.is_empty(), "makiba gives server.stop no param");
    assert!(
        stop.result.is_empty(),
        "makiba reads no field of server.stop"
    );

    let def = vendored();
    let mut without = def.clone();
    methods_mut(&mut without).retain(|m| m["name"] != "server.stop");
    let found = problems(&without, USES);
    let want = "server.stop: the method is not in the definition".to_string();
    assert!(found.contains(&want), "want {want:?}; got {found:?}");

    let mut with_param = def.clone();
    for m in methods_mut(&mut with_param) {
        if m["name"] == "server.stop" {
            m["params"].as_array_mut().unwrap().push(json!({
                "name": "force",
                "required": true,
                "schema": {"type": "boolean"}
            }));
        }
    }
    let found = problems(&with_param, USES);
    let want = "server.stop: param force is required and makiba does not give it".to_string();
    assert!(found.contains(&want), "want {want:?}; got {found:?}");

    let found = problems(&def, USES);
    assert!(found.is_empty(), "the pinned release holds: {found:?}");
}

// ---- the reader itself, on a small definition ---------------------------------------------------

fn small() -> Value {
    json!({
        "openrpc": "1.3.2",
        "info": {"title": "t", "version": PINNED},
        "components": {"schemas": {
            "Core": {"type": "object", "properties": {"name": {"type": "string"}}},
            "Base": {"type": "object", "properties": {"version": {"type": "string"}}},
            "Item": {"type": "object", "properties": {"labels": {"type": "object"}}}
        }},
        "methods": [
            {"name": "m", "params": [
                {"name": "a", "schema": {"type": "string"}},
                {"name": "b", "required": true, "schema": {"type": "integer"}}],
             "result": {"name": "r", "schema": {"allOf": [
                {"$ref": "#/components/schemas/Base"},
                {"type": "object", "properties": {
                    "core": {"$ref": "#/components/schemas/Core"},
                    "items": {"type": "array", "items": {"$ref": "#/components/schemas/Item"}}}}]}}}
        ]
    })
}

const GOOD: Use = Use {
    method: "m",
    params: &["a", "b"],
    result: &[
        Field {
            path: "version",
            ty: "string",
        },
        Field {
            path: "core.name",
            ty: "string",
        },
        Field {
            path: "items[].labels",
            ty: "object",
        },
    ],
};

#[test]
fn the_reader_follows_refs_all_of_and_arrays() {
    assert_eq!(problems(&small(), &[GOOD]), Vec::<String>::new());
}

#[test]
fn the_reader_names_each_kind_of_miss() {
    let def = small();
    let one = |u: Use| problems(&def, &[u]);
    let with = |result: &'static [Field]| Use {
        method: "m",
        params: &["a", "b"],
        result,
    };
    assert_eq!(
        one(with(&[Field {
            path: "core.id",
            ty: ""
        }])),
        ["m: result field core.id (no such field)"]
    );
    assert_eq!(
        one(with(&[Field {
            path: "version",
            ty: "integer"
        }])),
        [r#"m: result field version is {"string"}, makiba reads integer"#]
    );
    assert_eq!(
        one(with(&[Field {
            path: "version[].x",
            ty: ""
        }])),
        ["m: result field version[].x (not an array)"]
    );
    assert_eq!(
        one(Use {
            method: "m",
            params: &["a"],
            result: &[]
        }),
        ["m: param b is required and makiba does not give it"]
    );
    assert_eq!(
        one(Use {
            method: "m",
            params: &["a", "b", "c"],
            result: &[]
        }),
        ["m: param c is not in the definition"]
    );
    assert_eq!(
        one(Use {
            method: "gone",
            params: &[],
            result: &[]
        }),
        ["gone: the method is not in the definition"]
    );
}

#[test]
fn a_ref_that_leads_nowhere_is_reported_not_skipped() {
    let mut def = small();
    def["components"]["schemas"]["Core"] = json!({"$ref": "#/components/schemas/Missing"});
    let found = problems(&def, &[GOOD]);
    assert_eq!(
        found,
        ["m: result field core.name (unresolved $ref #/components/schemas/Missing)"]
    );
}
