//! How the git hooks read a command line: split it into simple commands, unwrap `eval` and
//! `bash -c`, collect assignments, expand the variables they set, and flatten command
//! substitutions. Ported from claude-org-ja's shared hook library (`.hooks/lib/`), function by
//! function, so that its tests' cases hold for the port. Like the original it is not a shell: it
//! reads enough of the syntax to find where a command starts, and when it cannot follow the syntax
//! it falls back to a split on every `;`, `&`, `|` and newline, which leans to refusing.

use super::re::Re;

/// The text of `command` as the original hooks receive it: a trailing newline is dropped, as a
/// shell's command substitution and awk's records both drop it.
fn records(text: &str) -> String {
    text.strip_suffix('\n').unwrap_or(text).to_string()
}

/// The segments of `command` (one per simple command), and whether the parse was lost. A lost
/// parse falls back to splitting on every separator.
pub fn split_segments(command: &str) -> (Vec<String>, bool) {
    let mut s = Splitter::new(&records(command));
    s.run();
    (s.segs, s.lost)
}

/// The segments of `command`, each cut into its lines, as the hooks read them.
pub fn segment_lines(command: &str) -> Vec<String> {
    split_segments(command)
        .0
        .iter()
        .flat_map(|s| s.split('\n').map(String::from).collect::<Vec<_>>())
        .collect()
}

/// True when `set` holds `c`; the end of the text (`'\0'`) is in every set, as awk's
/// `index(set, "")` is 1.
fn in_set(set: &str, c: char) -> bool {
    c == '\0' || set.contains(c)
}

const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "{", "time", "for",
];

struct Splitter {
    /// The text, with a dummy character at 0 so that positions count from 1 as in awk.
    buf: Vec<char>,
    n: usize,
    orig: Vec<char>,
    i: usize,
    st: Vec<char>,
    seg: String,
    segs: Vec<String>,
    // Here-documents waiting for their bodies: delimiter, strip tabs, unquoted, stack depth.
    hd: Vec<(String, bool, bool, usize)>,
    redir: bool,
    ar: bool,
    apd: i64,
    lost: bool,
    ws: bool,
    cp: bool,
    ap: bool,
    wb: isize,
    cpw: bool,
    apw: bool,
    tm: bool,
}

impl Splitter {
    fn new(text: &str) -> Splitter {
        let mut buf = vec!['\0'];
        buf.extend(text.chars());
        let n = buf.len() - 1;
        Splitter {
            orig: buf[1..].to_vec(),
            buf,
            n,
            i: 1,
            st: vec![],
            seg: String::new(),
            segs: vec![],
            hd: vec![],
            redir: false,
            ar: false,
            apd: 0,
            lost: false,
            ws: true,
            cp: true,
            ap: true,
            wb: 0,
            cpw: false,
            apw: false,
            tm: false,
        }
    }

    fn at(&self, j: usize) -> char {
        if j >= 1 && j <= self.n {
            self.buf[j]
        } else {
            '\0'
        }
    }

    fn sub(&self, j: usize, len: usize) -> String {
        (j..j + len)
            .map(|k| self.at(k))
            .filter(|&c| c != '\0')
            .collect()
    }

    fn add(&mut self, c: char) {
        if c != '\0' {
            self.seg.push(c);
        }
    }

    fn sp(&self) -> usize {
        self.st.len()
    }

    fn top(&self) -> char {
        self.st.last().copied().unwrap_or(' ')
    }

    fn emit(&mut self) {
        self.segs.push(std::mem::take(&mut self.seg));
    }

    fn frame_start(&mut self) {
        self.ws = true;
        self.cp = true;
        self.ap = true;
        self.wb = 0;
    }

    fn unsplice(&mut self, p: usize) {
        while self.at(p) == '\\' && self.at(p + 1) == '\n' {
            self.buf.drain(p..p + 2);
            self.n -= 2;
        }
    }

    fn arith(&self) -> bool {
        for &t in self.st.iter().rev() {
            if t == 'c' {
                return false;
            }
            if t == 'r' {
                return true;
            }
        }
        self.ar
    }

    fn start_word(&mut self) {
        if self.wb == 0 {
            self.wb = self.i as isize;
            self.cpw = self.cp;
            self.apw = self.ap;
        }
    }

    fn end_word(&mut self) {
        if self.wb == 0 {
            return;
        }
        let w = if self.wb > 0 {
            let wb = self.wb as usize;
            self.sub(wb, self.i - wb)
        } else {
            String::new()
        };
        if self.sp() > 0 && (w == "case" || w == "esac") {
            self.lost = true;
        }
        if w == "coproc" {
            self.lost = true;
        }
        let to = self.tm && (w == "-p" || w == "--");
        self.cp = self.cpw && (KEYWORDS.contains(&w.as_str()) || to);
        self.tm = self.cp && (w == "time" || (to && w == "-p"));
        self.ap = (self.cp && w != "for") || (self.apw && assignment_word(&w));
        self.wb = 0;
    }

    fn queue_heredoc(&mut self, mut j: usize) {
        let mut strip = false;
        let mut quoted = false;
        self.unsplice(j);
        if self.at(j) == '-' {
            strip = true;
            j += 1;
        }
        while self.at(j) == ' ' || self.at(j) == '\t' || self.sub(j, 2) == "\\\n" {
            self.unsplice(j);
            if self.at(j) == ' ' || self.at(j) == '\t' {
                j += 1;
            }
        }
        let mut d = String::new();
        let mut done = false;
        while j <= self.n && !done {
            self.unsplice(j);
            let ch = self.at(j);
            if ch == '$' && self.at(j + 1) == '"' {
                j += 1;
                continue;
            }
            if ch == '`' || (ch == '$' && in_set("'{[(", self.at(j + 1))) {
                self.lost = true;
                break;
            }
            if ch == '\'' {
                quoted = true;
                j += 1;
                while j <= self.n && self.at(j) != '\'' {
                    d.push(self.at(j));
                    j += 1;
                }
                j += 1;
            } else if ch == '"' {
                quoted = true;
                j += 1;
                while j <= self.n && self.at(j) != '"' {
                    if self.at(j) == '\\' && in_set("$`\"\\\n", self.at(j + 1)) {
                        j += 1;
                        if self.at(j) == '\n' {
                            j += 1;
                            continue;
                        }
                    }
                    let c = self.at(j);
                    if c != '\0' {
                        d.push(c);
                    }
                    j += 1;
                }
                j += 1;
            } else if ch == '\\' {
                quoted = true;
                let c = self.at(j + 1);
                if c != '\0' {
                    d.push(c);
                }
                j += 2;
            } else if in_set(" \t\n;&|()<>", ch) {
                done = true;
            } else {
                d.push(ch);
                j += 1;
            }
        }
        if let Some(last) = self.hd.last()
            && last.3 != self.sp()
        {
            self.lost = true;
        }
        self.hd.push((d, strip, !quoted, self.sp()));
    }

    fn read_heredocs(&mut self) {
        let mut j = self.i + 1;
        let docs = std::mem::take(&mut self.hd);
        for (delim, strip, unquoted, _) in &docs {
            let mut found = false;
            while j <= self.n {
                let mut cmp = String::new();
                let mut pl = 0;
                while j <= self.n {
                    let mut e = j;
                    while e <= self.n && self.at(e) != '\n' {
                        e += 1;
                    }
                    let mut line = self.sub(j, e - j);
                    self.seg.push('\n');
                    self.seg.push_str(&line);
                    j = e + 1;
                    let first = pl == 0;
                    pl += 1;
                    if first && *strip {
                        line = line.trim_start_matches('\t').to_string();
                    }
                    let trailing = line.chars().rev().take_while(|&c| c == '\\').count();
                    if *unquoted && e <= self.n && trailing % 2 == 1 {
                        cmp.push_str(&line[..line.len() - 1]);
                        continue;
                    }
                    cmp.push_str(&line);
                    break;
                }
                if cmp == *delim {
                    found = true;
                    break;
                }
            }
            if !found {
                self.lost = true;
            }
        }
        self.i = if j > self.n + 1 { self.n + 1 } else { j - 1 };
    }

    fn push(&mut self, t: char) {
        self.st.push(t);
    }

    fn run(&mut self) {
        while self.i <= self.n {
            let t = self.top();
            if !matches!(t, 's' | 'a' | 'b') {
                self.unsplice(self.i);
                if in_set("$<>&|()", self.at(self.i)) {
                    self.unsplice(self.i + 1);
                    let two = self.sub(self.i, 2);
                    if two == "$(" || two == "<<" {
                        self.unsplice(self.i + 2);
                    }
                }
                if self.i > self.n {
                    break;
                }
            }
            let i = self.i;
            let c = self.at(i);
            let nc = self.at(i + 1);
            if t == 's' {
                self.add(c);
                self.i += 1;
                if c == '\'' {
                    self.st.pop();
                }
                continue;
            }
            if t == 'a' || t == 'b' {
                if c == '\\' {
                    self.add(c);
                    self.add(nc);
                    self.i += 2;
                    continue;
                }
                self.add(c);
                self.i += 1;
                if (t == 'a' && c == '\'') || (t == 'b' && c == '`') {
                    self.st.pop();
                }
                continue;
            }
            if matches!(t, 'd' | 'e' | 'k') {
                if c == '\\' {
                    if nc != '\n' {
                        self.add(c);
                        self.add(nc);
                    }
                    self.i += 2;
                    continue;
                }
                if (t == 'd' && c == '"') || (t == 'e' && c == '}') || (t == 'k' && c == ']') {
                    self.st.pop();
                    self.add(c);
                    self.i += 1;
                    continue;
                }
                let below = if self.sp() >= 2 {
                    self.st[self.sp() - 2]
                } else {
                    ' '
                };
                if t == 'e' && below == 'd' && (c == '\'' || (c == '$' && nc == '\'')) {
                    self.lost = true;
                }
                if t != 'd' && c == '\'' {
                    self.push('s');
                    self.add(c);
                    self.i += 1;
                    continue;
                }
                if t != 'd' && c == '"' {
                    self.push('d');
                    self.add(c);
                    self.i += 1;
                    continue;
                }
                if t != 'd' && c == '$' && nc == '\'' {
                    self.push('a');
                    self.add(c);
                    self.add(nc);
                    self.i += 2;
                    continue;
                }
                if c == '$' && nc == '(' {
                    if self.at(i + 2) == '(' {
                        self.push('r');
                        self.seg.push_str("$((");
                        self.i += 3;
                    } else {
                        self.push('c');
                        self.seg.push_str("$(");
                        self.i += 2;
                    }
                    self.frame_start();
                    continue;
                }
                if c == '$' && nc == '{' {
                    self.push('e');
                    self.seg.push_str("${");
                    self.i += 2;
                    continue;
                }
                if c == '$' && nc == '[' {
                    self.push('k');
                    self.seg.push_str("$[");
                    self.i += 2;
                    continue;
                }
                if c == '`' {
                    self.push('b');
                    self.add(c);
                    self.i += 1;
                    continue;
                }
                self.add(c);
                self.i += 1;
                continue;
            }
            let pr = self.redir;
            self.redir = false;
            if c == '\\' {
                if nc == '\n' {
                    self.i += 2;
                    continue;
                }
                self.start_word();
                self.ws = false;
                self.add(c);
                self.add(nc);
                self.i += 2;
                continue;
            }
            if self.ws && c == '#' && self.arith() {
                self.lost = true;
            } else if self.ws && c == '#' {
                while self.i <= self.n && self.at(self.i) != '\n' {
                    let ch = self.at(self.i);
                    self.add(ch);
                    self.i += 1;
                }
                continue;
            }
            if c == '"'
                || c == '\''
                || c == '`'
                || (c == '$' && matches!(nc, '\'' | '{' | '[' | '('))
            {
                self.start_word();
                self.ws = false;
                match c {
                    '"' => {
                        self.push('d');
                        self.add(c);
                        self.i += 1;
                    }
                    '\'' => {
                        self.push('s');
                        self.add(c);
                        self.i += 1;
                    }
                    '`' => {
                        self.push('b');
                        self.add(c);
                        self.i += 1;
                    }
                    _ => match nc {
                        '\'' => {
                            self.push('a');
                            self.seg.push_str("$'");
                            self.i += 2;
                        }
                        '{' => {
                            self.push('e');
                            self.seg.push_str("${");
                            self.i += 2;
                        }
                        '[' => {
                            self.push('k');
                            self.seg.push_str("$[");
                            self.i += 2;
                        }
                        _ => {
                            if self.at(i + 2) == '(' {
                                self.push('r');
                                self.seg.push_str("$((");
                                self.i += 3;
                            } else {
                                self.push('c');
                                self.seg.push_str("$(");
                                self.i += 2;
                            }
                            self.frame_start();
                        }
                    },
                }
                continue;
            }
            if c == ' ' || c == '\t' {
                self.end_word();
                self.ws = true;
                self.add(c);
                self.i += 1;
                continue;
            }
            if c == '['
                && self.wb > 0
                && self.apw
                && is_name(&self.sub(self.wb as usize, i - self.wb as usize))
            {
                self.push('k');
                self.add(c);
                self.i += 1;
                continue;
            }
            if c == '\n' {
                self.end_word();
                if !self.hd.is_empty() {
                    self.read_heredocs();
                    continue;
                }
                if self.sp() == 0 {
                    self.emit();
                } else {
                    self.add(c);
                }
                self.i += 1;
                self.ws = true;
                self.cp = true;
                self.ap = true;
                continue;
            }
            if c == ';' || c == '|' || c == '&' {
                self.end_word();
                if c == '&' && nc != '&' && (pr || nc == '>') {
                    self.add(c);
                    self.i += 1;
                    self.ws = true;
                    continue;
                }
                let w = if (c == '&' && nc == '&') || (c == '|' && (nc == '|' || nc == '&')) {
                    2
                } else {
                    1
                };
                if self.sp() == 0 {
                    self.emit();
                } else {
                    let s = self.sub(i, w);
                    self.seg.push_str(&s);
                }
                self.i += w;
                self.ws = true;
                self.cp = true;
                self.ap = true;
                continue;
            }
            if c == '<' || c == '>' {
                self.end_word();
                if c == '<' && nc == '<' && self.at(i + 2) == '<' {
                    self.seg.push_str("<<<");
                    self.i += 3;
                    self.ws = true;
                    continue;
                }
                if c == '<' && nc == '<' && self.arith() {
                    self.lost = true;
                    self.seg.push_str("<<");
                    self.i += 2;
                    self.ws = true;
                    continue;
                }
                if c == '<' && nc == '<' {
                    self.queue_heredoc(i + 2);
                    self.seg.push_str("<<");
                    self.i += 2;
                    self.ws = true;
                    continue;
                }
                self.add(c);
                self.i += 1;
                self.ws = true;
                self.redir = true;
                continue;
            }
            if c == '(' {
                self.end_word();
                if self.cp && nc == '(' && self.sp() > 0 {
                    self.push('r');
                    self.seg.push_str("((");
                    self.i += 2;
                    self.frame_start();
                    continue;
                }
                if self.cp && nc == '(' && self.sp() == 0 && !self.ar {
                    self.ar = true;
                    self.apd = 2;
                    self.seg.push_str("((");
                    self.i += 2;
                    self.ws = true;
                    continue;
                }
                if self.sp() > 0 {
                    self.push('p');
                    self.add(c);
                    self.i += 1;
                    self.frame_start();
                    continue;
                }
                if self.ar {
                    self.apd += 1;
                }
                self.add(c);
                self.i += 1;
                self.ws = true;
                self.cp = true;
                self.ap = true;
                continue;
            }
            if c == ')' {
                self.end_word();
                if t == 'r' {
                    if nc != ')' {
                        self.lost = true;
                    }
                    self.st.pop();
                    if nc == ')' {
                        self.seg.push_str("))");
                        self.i += 2;
                    } else {
                        self.add(c);
                        self.i += 1;
                    }
                    self.ws = false;
                    self.wb = -1;
                    self.cpw = false;
                    self.apw = false;
                    continue;
                }
                if t == 'c' || t == 'p' {
                    if self.hd.last().is_some_and(|h| h.3 >= self.sp()) {
                        self.lost = true;
                    }
                    self.st.pop();
                    self.add(c);
                    self.i += 1;
                    if t == 'c' {
                        self.ws = false;
                        self.wb = -1;
                        self.cpw = false;
                        self.apw = false;
                    } else {
                        self.ws = true;
                        self.cp = true;
                        self.ap = true;
                    }
                    continue;
                }
                if self.ar && self.sp() == 0 && self.apd == 2 && nc != ')' {
                    self.lost = true;
                }
                if self.ar && self.sp() == 0 {
                    self.apd -= 1;
                    if self.apd <= 0 {
                        self.ar = false;
                    }
                }
                self.add(c);
                self.i += 1;
                self.ws = true;
                self.cp = true;
                self.ap = true;
                continue;
            }
            self.start_word();
            self.ws = false;
            self.add(c);
            self.i += 1;
        }
        self.end_word();
        if !self.seg.is_empty() {
            self.emit();
        }
        if self.sp() > 0 || self.lost {
            self.lost = true;
            self.segs.clear();
            let orig = self.orig.clone();
            self.fallback(&orig);
            let mut joined = Vec::new();
            let mut bs = 0;
            let mut k = 0;
            while k < orig.len() {
                let c = orig[k];
                if c == '\\' && orig.get(k + 1) == Some(&'\n') && bs % 2 == 0 {
                    k += 2;
                    bs = 0;
                    continue;
                }
                joined.push(c);
                bs = if c == '\\' { bs + 1 } else { 0 };
                k += 1;
            }
            if joined != orig {
                self.fallback(&joined);
            }
        }
    }

    fn fallback(&mut self, s: &[char]) {
        self.seg.clear();
        for &ch in s {
            if matches!(ch, ';' | '&' | '|' | '\n') {
                self.emit();
            } else {
                self.seg.push(ch);
            }
        }
        if !self.seg.is_empty() {
            self.emit();
        }
    }
}

fn is_name(w: &str) -> bool {
    let mut c = w.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// `^[A-Za-z_][A-Za-z0-9_]*(\[.*\])?\+?=`
fn assignment_word(w: &str) -> bool {
    Re::new(r"^[A-Za-z_][A-Za-z0-9_]*(\[.*\])?\+?=").is_match(w)
}

/// `flatten_substitutions`: the line, then the body of each command substitution after it. When
/// the substitutions cannot be read (a backslash, or one that does not close), the line with its
/// separators turned to spaces is added too. Quotes become spaces.
pub fn flatten(line: &str) -> String {
    let arith = Re::new(r"\$\(\([^()]*\)\)");
    let subst = Re::new(r"\$\([^()]*\)");
    let tick = Re::new(r"`[^`]*`");
    let mut out = line.to_string();
    let s = arith.replace_all(line, "");
    let left = tick.replace_all(&subst.replace_all(&s, ""), "");
    let undet = line.contains('\\') || left.contains("$(") || left.contains('`');
    for (re, skip, s) in [(&subst, 2, s.as_str()), (&tick, 1, line)] {
        let chars: Vec<char> = s.chars().collect();
        let mut rest = &chars[..];
        while let Some((a, b)) = re.find_chars(rest) {
            out.push(' ');
            out.extend(&rest[a + skip..b - 1]);
            rest = &rest[b..];
        }
    }
    if undet {
        let d = Re::new(r"[$()`;&|]").replace_all(line, " ");
        let d = Re::new(r#"[\\"']"#).replace_all(&d, "");
        out.push(' ');
        out.push_str(&d);
    }
    Re::new(r#"['"]"#).replace_all(&out, " ")
}

/// `flatten_substitutions` over a text of several lines.
pub fn flatten_text(text: &str) -> String {
    records(text)
        .split('\n')
        .map(flatten)
        .collect::<Vec<_>>()
        .join("\n")
}

fn emit_assign(out: &mut Vec<String>, var: &str, val: &str) {
    let mut flat = val.to_string();
    for (re, skip) in [(Re::new(r"\$\([^()]*\)"), 2), (Re::new(r"`[^`]*`"), 1)] {
        let chars: Vec<char> = val.chars().collect();
        let mut rest = &chars[..];
        while let Some((a, b)) = re.find_chars(rest) {
            flat.push(' ');
            flat.extend(&rest[a + skip..b - 1]);
            rest = &rest[b..];
        }
    }
    out.push(format!(
        "{var}={}",
        Re::new(r#"['"]"#).replace_all(&flat, " ")
    ));
}

/// `collect_assignments`: every `NAME=value` at the start of a line (after `export`), with the
/// value read as the shell would and its substitutions flattened, as `NAME=value` strings.
pub fn collect_assignments(lines: &[String]) -> Vec<String> {
    let name_eq = Re::new(r"^[A-Za-z_][A-Za-z0-9_]*=");
    let export = Re::new(r"^export[ \t]+");
    let next_assign = Re::new(r#"[ \t;&|(`'"][A-Za-z_][A-Za-z0-9_]*="#);
    let mut out = Vec::new();
    for line in lines {
        let mut seg: Vec<char> = line.trim_start_matches([' ', '\t']).chars().collect();
        if let Some((_, e)) = export.find_chars(&seg) {
            seg = seg[e..].to_vec();
            while matches!(seg.first(), Some(' ' | '\t')) {
                seg.remove(0);
            }
        }
        while let Some((_, e)) = name_eq.find_chars(&seg) {
            let var: String = seg[..e - 1].iter().collect();
            let rest: Vec<char> = seg[e..].to_vec();
            let n = rest.len();
            let mut val = String::new();
            let (mut dq, mut sq, mut bt) = (false, false, false);
            let mut depth = 0i64;
            let mut undet = false;
            let mut i = 0;
            while i < n {
                let c = rest[i];
                let next = rest.get(i + 1).copied().unwrap_or('\0');
                if sq {
                    if c == '\'' {
                        sq = false;
                        i += 1;
                        continue;
                    }
                    val.push(c);
                    i += 1;
                    continue;
                }
                if c == '\\' || (c == '$' && next == '\'') {
                    undet = true;
                }
                if dq {
                    if c == '"' {
                        if depth > 0 {
                            undet = true;
                        }
                        dq = false;
                        i += 1;
                        continue;
                    }
                    if c == '$' && next == '(' {
                        depth += 1;
                        val.push_str("$(");
                        i += 2;
                        continue;
                    }
                    if depth > 0 {
                        if c == '(' {
                            depth += 1;
                        }
                        if c == ')' {
                            depth -= 1;
                        }
                    }
                    val.push(c);
                    i += 1;
                    continue;
                }
                if bt {
                    if c == '`' {
                        bt = false;
                    }
                    val.push(c);
                    i += 1;
                    continue;
                }
                if c == '"' {
                    dq = true;
                    i += 1;
                    continue;
                }
                if c == '\'' {
                    sq = true;
                    i += 1;
                    continue;
                }
                if c == '`' {
                    bt = true;
                    val.push(c);
                    i += 1;
                    continue;
                }
                if c == '$' && next == '(' {
                    depth += 1;
                    val.push_str("$(");
                    i += 2;
                    continue;
                }
                if depth > 0 {
                    if c == '(' {
                        depth += 1;
                    }
                    if c == ')' {
                        depth -= 1;
                    }
                    val.push(c);
                    i += 1;
                    continue;
                }
                if c == ' ' || c == '\t' {
                    break;
                }
                val.push(c);
                i += 1;
            }
            if dq || sq || bt || depth > 0 {
                undet = true;
            }
            if undet {
                let r: String = rest.iter().collect();
                let r = Re::new(r#"[\\"']"#).replace_all(&r, "");
                emit_assign(&mut out, &var, &r);
                let mut r: Vec<char> = r.chars().collect();
                while let Some((a, _)) = next_assign.find_chars(&r) {
                    r = r[a + 1..].to_vec();
                    let (_, e) = name_eq.find_chars(&r).unwrap();
                    let v: String = r[..e - 1].iter().collect();
                    let val: String = r[e..].iter().collect();
                    emit_assign(&mut out, &v, &val);
                    r = r[e..].to_vec();
                }
                break;
            }
            if !val.is_empty() {
                emit_assign(&mut out, &var, &val);
            }
            seg = rest.get(i + 1..).unwrap_or(&[]).to_vec();
            while matches!(seg.first(), Some(' ' | '\t')) {
                seg.remove(0);
            }
        }
    }
    out
}

/// `expand_known_vars`: `${NAME}` and `$NAME` replaced by each collected value, in order.
pub fn expand(segment: &str, assignments: &[String]) -> String {
    let mut segment = segment.to_string();
    for pair in assignments {
        let (var, val) = pair.split_once('=').unwrap_or((pair, ""));
        segment = segment.replace(&format!("${{{var}}}"), val);
        let chars: Vec<char> = segment.chars().collect();
        let v: Vec<char> = var.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '$' && i + 1 < chars.len() {
                let rest = &chars[i + 1..];
                if rest.starts_with(&v)
                    && rest
                        .get(v.len())
                        .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == '_'))
                {
                    out.push_str(val);
                    i += 1 + v.len();
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        segment = out;
    }
    segment
}

/// One pass of `unwrap_eval_and_bashc` over one line: the bodies of its `eval`, `bash -c` and
/// `sh -c`; a body it cannot read is given up as the rest of the line, split at separators.
fn unwrap_pass(line: &str, out: &mut Vec<String>, lost: &mut bool) {
    let head_re = Re::new(r"(^|[^A-Za-z0-9_-])(eval|bash[ \t]+-c|sh[ \t]+-c)[ \t]+");
    let word_re = Re::new(r"^[^ \t;&|`][^ \t;&|`]*");
    let after_re = Re::new(r"^([ \t;&|)<>]|$)");
    let give_up = |arg: &[char], out: &mut Vec<String>, lost: &mut bool| {
        let a: String = arg.iter().collect();
        let a = Re::new(r#"[\\"']"#).replace_all(&a, "");
        let a = a.replace('$', " $");
        let a = Re::new(r"[;&|()`]").replace_all(&a, "\n");
        if !a.is_empty() {
            out.extend(a.split('\n').map(String::from));
        }
        *lost = true;
    };
    let mut line: Vec<char> = line.chars().collect();
    while let Some((a, b)) = head_re.find_chars(&line) {
        let head: String = line[a..b].iter().collect();
        let arg: Vec<char> = line[b..].to_vec();
        line = arg.clone();
        let q = arg.first().copied().unwrap_or('\0');
        if q == '"' || q == '\'' {
            let close = arg[1..].iter().position(|&c| c == q);
            let Some(close) = close else {
                give_up(&arg, out, lost);
                break;
            };
            let body: String = arg[1..close + 1].iter().collect();
            line = arg[close + 2..].to_vec();
            if body.contains('\\') || after_re.find_chars(&line).is_none() {
                give_up(&arg, out, lost);
                break;
            }
            if !body.is_empty() {
                out.push(body);
            }
            continue;
        }
        if q == '$' && matches!(arg.get(1), Some('\'' | '"')) {
            give_up(&arg, out, lost);
            break;
        }
        let Some((_, e)) = word_re.find_chars(&arg) else {
            continue;
        };
        let word: String = arg[..e].iter().collect();
        if word.contains(['\\', '"', '\'']) {
            give_up(&arg, out, lost);
            break;
        }
        if head.contains("eval") && !word.is_empty() {
            out.push(word);
        }
    }
}

/// `unwrap_eval_and_bashc`: the bodies found in `lines`, then in those bodies, up to three
/// levels, every level's lines in order.
pub fn unwrap(lines: &[String]) -> Vec<String> {
    let mut current: Vec<String> = lines.to_vec();
    // The shell's `$(cat)` drops trailing empty lines.
    while current.last().is_some_and(|l| l.is_empty()) {
        current.pop();
    }
    let mut all = Vec::new();
    if current.is_empty() {
        return all;
    }
    for _ in 0..3 {
        let mut next = Vec::new();
        let mut lost = false;
        for l in &current {
            unwrap_pass(l, &mut next, &mut lost);
        }
        while next.last().is_some_and(|l| l.is_empty()) {
            next.pop();
        }
        if next.is_empty() {
            break;
        }
        all.extend(next.iter().cloned());
        current = next;
    }
    all
}

/// The segments a hook checks, as the carried hooks build them: the segment lines, then the
/// bodies unwrapped from them; with the assignments collected from both.
pub fn segments_and_assignments(command: &str) -> (Vec<String>, Vec<String>) {
    let mut segs = segment_lines(command);
    let unwrapped = unwrap(&segs);
    segs.extend(unwrapped.into_iter().filter(|l| !l.is_empty()));
    let assigns = collect_assignments(&segs)
        .into_iter()
        .filter(|a| !a.is_empty())
        .collect();
    (segs, assigns)
}

/// `text` with every word that names git by a path (`/usr/bin/git`, `C:\git.exe`, `git.exe`)
/// written as `git`, so that the patterns that look for the word `git` see it. The original hooks
/// missed a path-qualified git; this is makiba's addition.
pub fn plain_git(text: &str) -> String {
    plain(text, "git")
}

/// `text` with every word that names the program `name` by a path written as `name`.
pub fn plain(text: &str, name: &str) -> String {
    let exe = format!("{name}.exe");
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        let lower = word.to_lowercase();
        let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
        if (base == name || base == exe) && lower != name {
            out.push_str(name);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_whitespace() {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Why the git hooks cannot read the command of `segments` as the shell will run it, if they
/// cannot (makiba's addition; the reading here is text, see Issue 140):
///
/// - git's subcommand word holds a quote or a backslash (`re"set"`), which the flattening above
///   would split into two words;
/// - a variable is assigned more than once, so which value a later `$NAME` has depends on
///   where it is used, and the expansion above takes the first.
///
/// Both lean to refusing.
pub fn unreadable(segments: &[String], assigns: &[String]) -> Option<String> {
    unreadable_at(segments, assigns).map(|(_, why)| why)
}

/// `unreadable`, with the text it could not read: `(matched, reason)`.
pub fn unreadable_at(segments: &[String], assigns: &[String]) -> Option<(String, String)> {
    let takes_value = |t: &str| {
        matches!(
            t,
            "-C" | "-c"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--exec-path"
                | "--super-prefix"
                | "--config-env"
                | "--attr-source"
        )
    };
    for segment in segments {
        let words: Vec<&str> = segment.split_whitespace().collect();
        for (i, w) in words.iter().enumerate() {
            if plain_git(w) != "git" {
                continue;
            }
            let mut j = i + 1;
            while j < words.len() && words[j].starts_with('-') {
                j += if takes_value(words[j]) { 2 } else { 1 };
            }
            if let Some(sub) = words.get(j)
                && sub.contains(['"', '\'', '\\'])
            {
                return Some((
                    format!("git {sub}"),
                    format!(
                        "git's subcommand {sub} is quoted or escaped, so the hooks cannot read it; write it plainly"
                    ),
                ));
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for a in assigns {
        let name = a.split('=').next().unwrap_or(a);
        if !seen.insert(name) {
            return Some((
                format!("{name}= twice"),
                format!(
                    "{name} is assigned more than once, so the hooks cannot tell its value where it is used; use one name per value"
                ),
            ));
        }
    }
    None
}

/// A segment with the known variables expanded and its substitutions flattened.
pub fn flat_segment(segment: &str, assignments: &[String]) -> String {
    let expanded = if assignments.is_empty() {
        segment.to_string()
    } else {
        expand(segment, assignments)
    };
    plain_git(&flatten_text(&expanded))
}
