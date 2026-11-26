// makiba P17 (docs/design/probes.md, P17): a herdr-app panel that runs two fixed makiba commands as the
// person and shows what they return. It is a probe tool, not part of makiba.
//
// It runs exactly two argument vectors, both constants below; the person chooses only where they run
// (a herdr machine, an optional prefix such as wsl.exe, and the makiba program). No shell line is ever
// built, nothing is retried, and no verb that changes state is offered.
import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextView } from "gpui-base";
import { exec, machines, theme, checking, window as appWindow } from "herdr";

// The only two commands this panel runs (P17 steps 4-5 and 9-11).
const STATUS_ARGS = Object.freeze(["status", "--json"]);
const CONFIG_ARGS = Object.freeze(["config", "get", "conversation_language"]);
const POLL_MS = 5000;
const RUN_TIMEOUT_MS = 20000;
const STORE = "makiba-p17.reach";

function loadReach() {
  try {
    const v = JSON.parse(localStorage.getItem(STORE) || "null");
    if (v && typeof v === "object") return v;
  } catch (_e) { /* a bad value starts from the defaults */ }
  return { machine: null, prefix: "[]", program: "makiba" };
}

// The prefix is a JSON array of strings, e.g. ["wsl.exe", "-e"]. Returns {argv} or {error}.
function parsePrefix(text) {
  let v;
  try { v = JSON.parse(text.trim() === "" ? "[]" : text); } catch (e) { return { error: "prefix is not JSON: " + e }; }
  if (!Array.isArray(v) || v.some((s) => typeof s !== "string" || s === "")) {
    return { error: "prefix must be a JSON array of non-empty strings" };
  }
  return { argv: v };
}

// The full argument vector: prefix, then the makiba program, then the fixed arguments.
function vector(reach, args) {
  const p = parsePrefix(reach.prefix);
  if (p.error) return p;
  const program = reach.program.trim();
  if (program === "") return { error: "the makiba program is empty" };
  const argv = [...p.argv, program, ...args];
  return { argv };
}

// Shown as a fenced block, with a fence longer than any backtick run inside, so output is literal.
function block(text) {
  const runs = (text.match(/`+/g) || []).map((s) => s.length);
  const fence = "`".repeat(Math.max(3, ...runs.map((n) => n + 1)));
  return fence + "\n" + text + "\n" + fence;
}

function quoteArgv(argv) {
  return argv.map((a) => JSON.stringify(a)).join(" ");
}

export default class MakibaP17 extends View {
  init(_props, cx) {
    this.reach = loadReach();
    this.prefixInput = InputState.new({ placeholder: '[] or ["wsl.exe", "-e"]', value: this.reach.prefix });
    this.programInput = InputState.new({ placeholder: "makiba", value: this.reach.program });
    this.prefixInput.on("change", (_e, cx) => { this.reach.prefix = this.prefixInput.value(); this.save(); cx.notify(); });
    this.programInput.on("change", (_e, cx) => { this.reach.program = this.programInput.value(); this.save(); cx.notify(); });
    this.urlInput = InputState.new({ placeholder: "http://127.0.0.1:<port>/<path>/ (the probe prints it)" });
    this.urlNote = "";
    // M1: the status poll.
    this.polling = false;
    this.pollTask = null;
    this.status = null; // {n, at, argv, code, stdout, stderr, error, running, heartbeat}
    this.statusRuns = 0;
    this.statusBusy = false;
    // M2: the one config read.
    this.config = null;
    this.configBusy = false;
  }

  save() {
    try { localStorage.setItem(STORE, JSON.stringify(this.reach)); } catch (_e) { /* not kept: fine */ }
  }

  async runOnce(args) {
    const v = vector(this.reach, args);
    const at = new Date().toLocaleTimeString();
    const on = this.reach.machine ?? null;
    if (v.error) return { at, on, argv: null, error: v.error };
    const [cmd, ...rest] = v.argv;
    try {
      const r = await exec(cmd, rest, { on, timeout: RUN_TIMEOUT_MS });
      return { at, on, argv: v.argv, code: r.code, stdout: r.stdout, stderr: r.stderr };
    } catch (e) {
      // exec rejects when the program cannot start, the machine is unreachable, or on timeout.
      return { at, on, argv: v.argv, error: "exec rejected: " + e };
    }
  }

  // One status read; the next tick starts only after this one ended (no overlap, no early retry).
  pollOnce(cx) {
    if (this.statusBusy) return;
    if (appWindow()?.visible === false) return;
    this.statusBusy = true;
    cx.spawn(async (cx) => {
      const r = await this.runOnce(STATUS_ARGS);
      this.statusRuns += 1;
      r.n = this.statusRuns;
      if (r.code === 0) {
        try {
          const d = JSON.parse(r.stdout);
          r.running = String(d?.makibad?.running);
          r.heartbeat = String(d?.makibad?.heartbeat_at);
        } catch (e) { r.parse = "stdout is not JSON: " + e; }
      }
      this.status = r;
      this.statusBusy = false;
      cx.notify();
    });
  }

  togglePoll(cx) {
    if (this.polling) {
      this.polling = false;
      if (this.pollTask) this.pollTask.cancel();
      this.pollTask = null;
    } else {
      if (checking()) return;
      this.polling = true;
      this.pollOnce(cx);
      this.pollTask = cx.timer.every(POLL_MS, (cx) => { if (this.polling) this.pollOnce(cx); });
    }
    cx.notify();
  }

  runConfig(cx) {
    if (this.configBusy || checking()) return;
    this.configBusy = true;
    cx.notify();
    cx.spawn(async (cx) => {
      this.config = await this.runOnce(CONFIG_ARGS);
      this.configBusy = false;
      cx.notify();
    });
  }

  openInBrowser(cx) {
    const url = this.urlInput.value().trim();
    if (!/^http:\/\/127\.0\.0\.1:\d+\//.test(url)) {
      this.urlNote = "Paste the http://127.0.0.1:<port>/... URL the probe printed.";
    } else {
      cx.open_url(url);
      this.urlNote = "Handed to the system browser (outside the app; not counted for m1-page or m3-page).";
    }
    cx.notify();
  }

  render(_cx) {
    const t = theme();
    const button = (id, label, onClick, enabled = true) =>
      div().id(id).px(10).py(4).rounded(4).border(1).border_color(t.input)
        .bg(enabled ? t.raised : t.surface).text_color(enabled ? t.foreground : t.dim)
        .when(enabled, (el) => el.hover((s) => s.bg(t.hover)).on_click((_e, cx) => onClick(cx)))
        .child(label);
    const heading = (text) => div().pt(10).font_semibold().text_size(14).child(text);
    const dim = (text) => div().text_color(t.muted).text_size(12).child(text);
    const result = (id, r) => {
      if (!r) return dim("Not run yet.");
      const rows = [dim(`at ${r.at}` + (r.n ? ` · run ${r.n}` : ""))];
      rows.push(dim("on: " + (r.on ?? "this computer")));
      if (r.argv) rows.push(dim("argv: " + quoteArgv(r.argv)));
      if (r.error) {
        rows.push(div().text_color(t.blocked).child(r.error));
        return v_flex().gap(4).children(rows);
      }
      rows.push(div().text_color(r.code === 0 ? t.foreground : t.blocked).child(`exit code: ${r.code}`));
      if (r.running !== undefined) rows.push(div().child(`makibad running: ${r.running} · heartbeat_at: ${r.heartbeat}`));
      if (r.parse) rows.push(div().text_color(t.blocked).child(r.parse));
      rows.push(dim("stdout:"));
      rows.push(TextView.markdown(id + "-out", block(r.stdout)).raw_html(false).selectable(true));
      rows.push(dim("stderr:"));
      rows.push(TextView.markdown(id + "-err", block(r.stderr === "" ? "(empty)" : r.stderr)).raw_html(false).selectable(true));
      return v_flex().gap(4).children(rows);
    };

    // This computer is always offered first; herdr's other machines follow.
    const ms = [{ name: null, layout: "ready" }, ...machines().filter((m) => m.name !== null)];
    const machineRow = h_flex().gap(6).flex_wrap().children(
      ms.map((m, i) => {
        const on = (m.name ?? null) === (this.reach.machine ?? null);
        return div().id(`machine-${i}`).px(8).py(2).rounded(4).border(1)
          .border_color(on ? t.accent : t.border).text_color(on ? t.foreground : t.muted)
          .hover((s) => s.bg(t.hover))
          .on_click((_e, cx) => { this.reach.machine = m.name ?? null; this.save(); cx.notify(); })
          .child((m.name ?? "this computer") + (m.layout === "unavailable" ? " (unavailable)" : ""));
      }));
    const sv = vector(this.reach, STATUS_ARGS);
    const cv = vector(this.reach, CONFIG_ARGS);
    const where = `on: ${this.reach.machine ?? "this computer"}`;

    return v_flex().id("root").size_full().bg(t.surface).text_color(t.foreground).text_size(13)
      .p(12).gap(6).overflow_y_scrollbar()
      .child(div().font_semibold().text_size(15).child("makiba P17"))
      .child(dim("Runs two fixed makiba commands as you and shows what they return. Nothing else."))

      .child(heading("Where the base is"))
      .child(dim("Way 1: pick the herdr machine the base runs on (exec's on) and leave the prefix []. Way 2: this computer, with a prefix such as wsl.exe."))
      .child(dim("Machine (the command runs there):"))
      .child(machineRow)
      .child(dim('Prefix, a JSON array (for the base in WSL from Windows, e.g. ["wsl.exe", "-e"]). Put only a way to reach the base here, never another command:'))
      .child(Input.new(this.prefixInput))
      .child(dim("The makiba program (with wsl.exe -e, an absolute path in WSL may be needed):"))
      .child(Input.new(this.programInput))

      .child(heading("M1 · steps 4-5 · m1-command"))
      .child(dim(sv.error ? sv.error : `Runs every ${POLL_MS / 1000} s, ${where}: ${quoteArgv(sv.argv)}`))
      .child(h_flex().gap(8).items_center()
        .child(button("poll", this.polling ? "Stop refreshing" : "Start refreshing status --json", (cx) => this.togglePoll(cx), !sv.error || this.polling))
        .child(dim(this.polling ? (this.statusBusy ? "running…" : "on") : "off")))
      .child(result("status", this.status))

      .child(heading("M2 · steps 9-11 · m2-act"))
      .child(dim(cv.error ? cv.error : `Runs once per click, ${where}: ${quoteArgv(cv.argv)}`))
      .child(button("config", this.configBusy ? "running…" : "Run config get conversation_language", (cx) => this.runConfig(cx), !cv.error && !this.configBusy))
      .child(result("config", this.config))

      .child(heading("M3 · steps 6-8 · m1-page, m3-page"))
      .child(div().text_color(t.blocked).child("Not possible through an addition: herdr-app's addition API has no web view, so a page cannot be shown inside the app. Record m1-page and m3-page as no (no web view) for this way."))
      .child(dim("The button below hands the URL to the system browser. That is outside the app and does not count for m1-page or m3-page."))
      .child(Input.new(this.urlInput))
      .child(button("browser", "Open in system browser (outside the app, not counted)", (cx) => this.openInBrowser(cx)))
      .when(this.urlNote !== "", (el) => el.child(dim(this.urlNote)));
  }

  async close(_cx) {
    this.polling = false;
    if (this.pollTask) this.pollTask.cancel();
  }
}
