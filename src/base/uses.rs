//! What makiba uses of herdr-core's API, as data: the operations, their parameters and the fields
//! of their results. `tests/base_compat.rs` checks every entry against the base's own definition
//! (its OpenRPC schema), so a move of the pin is a test run (docs/upgrading-the-base.md).
//!
//! Names are the base's. A path is dotted; `[]` after a name steps into the elements of an array
//! (`terminals[].labels`). `ty` is the JSON type makiba reads the field as; `""` means any.
//! An entry is added with the code that needs it: `Base` methods not built yet are listed from
//! spec section 6.2, and are corrected here when the real code meets the base.

/// A result field makiba reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub path: &'static str,
    pub ty: &'static str,
}

/// One operation: the parameters makiba gives, the result fields makiba reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Use {
    pub method: &'static str,
    pub params: &'static [&'static str],
    pub result: &'static [Field],
}

const fn f(path: &'static str, ty: &'static str) -> Field {
    Field { path, ty }
}

/// The terminal as `Base::get` and `Base::list` map it (spec section 6.1), under `terminals[].`
/// for `list`, at the top for `get`: kept as two literal lists so each path is greppable.
pub const USES: &[Use] = &[
    Use {
        method: "hello",
        params: &["generation"],
        result: &[f("version", "string"), f("generation", "integer")],
    },
    Use {
        method: "terminal.run",
        params: &[
            "argv",
            "cwd",
            "name",
            "labels",
            "env",
            "keep",
            "no_parent",
            "no_notify",
        ],
        result: &[f("address", "string")],
    },
    Use {
        method: "terminal.list",
        params: &["labels"],
        result: &[
            f("terminals[].address", "string"),
            f("terminals[].name", "string"),
            f("terminals[].labels", "object"),
            f("terminals[].state", "string"),
            f("terminals[].turn", "integer"),
            f("terminals[].exit_code", "integer"),
            f("terminals[].status", "object"),
            f("terminals[].status.state", "string"),
            f("terminals[].status.kind", "string"),
            f("terminals[].status.msg", "string"),
        ],
    },
    Use {
        method: "terminal.get",
        params: &["terminal"],
        result: &[
            f("address", "string"),
            f("name", "string"),
            f("labels", "object"),
            f("state", "string"),
            f("turn", "integer"),
            f("exit_code", "integer"),
            f("status", "object"),
            f("status.state", "string"),
            f("status.kind", "string"),
            f("status.msg", "string"),
        ],
    },
    Use {
        method: "terminal.read",
        params: &["terminal", "lines"],
        result: &[f("text", "string")],
    },
    Use {
        method: "terminal.keys",
        params: &["terminal", "keys"],
        result: &[],
    },
    Use {
        method: "terminal.send",
        params: &["terminal", "text", "if"],
        result: &[],
    },
    Use {
        method: "terminal.kill",
        params: &["terminal"],
        result: &[],
    },
    Use {
        method: "terminal.rm",
        params: &["terminal"],
        result: &[f("removed", "string")],
    },
    Use {
        method: "events.subscribe",
        params: &["labels"],
        result: &[f("seq", "integer"), f("terminals[].address", "string")],
    },
];
