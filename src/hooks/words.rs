//! A command line read as the shell splits it into words (Issue 300), for a hook that reads a
//! program's arguments rather than the whole text: quotes are removed and keep a word whole,
//! redirections and their targets are dropped, the bodies of here-documents are kept apart from
//! the words, and the bodies of command substitutions are handed back to be read as commands.
//!
//! It is not a shell. Variables are expanded only from the assignments the caller found in the
//! same command line, inside the words already split (a value is never read again as syntax: an
//! unquoted value splits at blanks, a quoted one stays one word). What it cannot follow (a quote,
//! a substitution or a here-document that does not close) is reported as `lost`, and the caller
//! leans to refusing.

use std::collections::BTreeMap;

/// The variables known to the reading: name to value, as the assignment's word gives it.
pub type Vars = BTreeMap<String, String>;

/// One word, after quote removal and expansion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    /// The word holds a command substitution (its text holds `$(...)` or a backquote as written).
    pub built: bool,
}

/// A here-document fed to a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heredoc {
    /// The delimiter was quoted: the body is not expanded.
    pub quoted: bool,
    pub body: String,
}

/// One simple command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cmd {
    pub words: Vec<Word>,
    /// The bodies of the command and process substitutions in its words and redirections.
    pub substs: Vec<String>,
    pub heredocs: Vec<Heredoc>,
    /// Its output goes to the next command through a pipe.
    pub piped: bool,
    /// It redirects its output to a file (`>`, `>>`, `&>`; not `>&2`).
    pub writes: bool,
}

/// A reading of a command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub cmds: Vec<Cmd>,
    /// A quote, a substitution or a here-document did not close.
    pub lost: bool,
}

/// Read `text` with the variables `vars`.
pub fn parse(text: &str, vars: &Vars) -> Parsed {
    let chars: Vec<char> = text.chars().collect();
    let mut lx = Lexer::new(&chars, vars);
    lx.run(0, false);
    lx.end_word();
    lx.end_cmd(false);
    Parsed {
        cmds: lx
            .cmds
            .into_iter()
            // An empty command that ends with a pipe is kept: `(cat <<EOF) | sh` pipes there.
            .filter(|c| {
                !c.words.is_empty() || !c.substs.is_empty() || !c.heredocs.is_empty() || c.piped
            })
            .collect(),
        lost: lx.lost,
    }
}

/// The assignments (`NAME=value`) that lead the commands of `parsed`, and after `export`,
/// `declare`, `local` or `readonly`: each name with its value, in order.
pub fn assignments(parsed: &Parsed) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for cmd in &parsed.cmds {
        let mut words = cmd.words.iter().peekable();
        if words
            .peek()
            .is_some_and(|w| matches!(w.text.as_str(), "export" | "declare" | "local" | "readonly"))
        {
            words.next();
        }
        for w in words {
            match assignment(&w.text) {
                Some((name, value)) => out.push((name.to_string(), value.to_string())),
                None => break,
            }
        }
    }
    out
}

/// `NAME=value` (or `NAME+=value`) split, or `None`.
pub fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    let name = name.strip_suffix('+').unwrap_or(name);
    let mut c = name.chars();
    let first = c.next()?;
    ((first.is_ascii_alphabetic() || first == '_')
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_'))
    .then_some((name, value))
}

/// The bodies of the command substitutions (`$(...)`, backquotes) in `text`, read as the lexer
/// reads them; `None` when one does not close, or a backslash makes the reading uncertain.
pub fn substitutions(text: &str) -> Option<Vec<String>> {
    if text.contains('\\') {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let vars = Vars::new();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'(') && chars.get(i + 2) != Some(&'(') {
            let mut lx = Lexer::new(&chars, &vars);
            let end = lx.run(i + 2, true);
            if lx.lost || end >= chars.len() {
                return None;
            }
            out.push(chars[i + 2..end].iter().collect());
            i = end + 1;
        } else if chars[i] == '`' {
            let end = (i + 1..chars.len()).find(|&j| chars[j] == '`')?;
            out.push(chars[i + 1..end].iter().collect());
            i = end + 1;
        } else {
            i += 1;
        }
    }
    Some(out)
}

struct Lexer<'a> {
    c: &'a [char],
    vars: &'a Vars,
    cmds: Vec<Cmd>,
    cur: Cmd,
    word: Option<Word>,
    /// The next word is a redirection's target, not an argument.
    target: bool,
    /// That target is a here-string's text.
    herestring: bool,
    /// Here-documents waiting for the end of the line: delimiter, strip tabs, quoted, command.
    pending: Vec<(String, bool, bool, usize)>,
    lost: bool,
}

impl<'a> Lexer<'a> {
    fn new(c: &'a [char], vars: &'a Vars) -> Self {
        Lexer {
            c,
            vars,
            cmds: Vec::new(),
            cur: Cmd::default(),
            word: None,
            target: false,
            herestring: false,
            pending: Vec::new(),
            lost: false,
        }
    }

    fn at(&self, i: usize) -> Option<char> {
        self.c.get(i).copied()
    }

    fn push(&mut self, ch: char) {
        self.word.get_or_insert_with(Word::default).text.push(ch);
    }

    fn push_str(&mut self, s: &str) {
        self.word.get_or_insert_with(Word::default).text.push_str(s);
    }

    fn end_word(&mut self) {
        if let Some(w) = self.word.take() {
            if self.target {
                self.target = false;
                // A here-string (`<<< word`) is the command's input, as a here-document is.
                if std::mem::take(&mut self.herestring) {
                    self.cur.heredocs.push(Heredoc {
                        quoted: true,
                        body: w.text,
                    });
                }
            } else {
                self.cur.words.push(w);
            }
        }
    }

    fn end_cmd(&mut self, piped: bool) {
        self.end_word();
        // A target never given (`> ;`) is the shell's error; it is not carried to the next command.
        self.target = false;
        self.herestring = false;
        let mut cmd = std::mem::take(&mut self.cur);
        cmd.piped = piped;
        self.cmds.push(cmd);
    }

    /// Read from `i`; with `stop`, up to the `)` that closes a substitution opened before `i`,
    /// whose position is returned. Without, to the end.
    fn run(&mut self, mut i: usize, stop: bool) -> usize {
        let mut depth = 0usize;
        while let Some(ch) = self.at(i) {
            match ch {
                '\\' => {
                    match self.at(i + 1) {
                        Some('\n') => {}
                        Some(n) => self.push(n),
                        None => self.push('\\'),
                    }
                    i += 2;
                }
                ' ' | '\t' => {
                    self.end_word();
                    i += 1;
                }
                '\n' => {
                    self.end_cmd(false);
                    i = self.heredoc_bodies(i + 1);
                }
                '#' if self.word.is_none() => {
                    while self.at(i).is_some_and(|c| c != '\n') {
                        i += 1;
                    }
                }
                ';' => {
                    self.end_cmd(false);
                    i += 1;
                }
                '|' => {
                    let or = self.at(i + 1) == Some('|');
                    self.end_cmd(!or);
                    i += if or || self.at(i + 1) == Some('&') {
                        2
                    } else {
                        1
                    };
                }
                '&' if self.at(i + 1) == Some('>') => {
                    self.end_word();
                    i = self.redirection(i + 1);
                }
                '&' => {
                    self.end_cmd(false);
                    i += if self.at(i + 1) == Some('&') { 2 } else { 1 };
                }
                '(' => {
                    depth += 1;
                    self.end_cmd(false);
                    i += 1;
                }
                ')' => {
                    if stop && depth == 0 {
                        self.end_cmd(false);
                        return i;
                    }
                    depth = depth.saturating_sub(1);
                    self.end_cmd(false);
                    i += 1;
                }
                '<' | '>' => {
                    if let Some(w) = &self.word
                        && !w.text.is_empty()
                        && w.text.chars().all(|c| c.is_ascii_digit())
                    {
                        // A descriptor (`2>`): not a word.
                        self.word = None;
                    } else {
                        self.end_word();
                    }
                    if self.at(i + 1) == Some('(') {
                        // Process substitution: a command whose output or input is a file name.
                        let end = self.substitution(i + 2);
                        self.push_str("/dev/fd/0");
                        if let Some(w) = self.word.as_mut() {
                            w.built = true;
                        }
                        i = end;
                    } else {
                        i = self.redirection(i);
                    }
                }
                '\'' => {
                    self.word.get_or_insert_with(Word::default);
                    let mut j = i + 1;
                    while let Some(c) = self.at(j) {
                        if c == '\'' {
                            break;
                        }
                        self.push(c);
                        j += 1;
                    }
                    if self.at(j).is_none() {
                        self.lost = true;
                    }
                    i = j + 1;
                }
                '"' => i = self.double_quoted(i + 1),
                '$' => i = self.dollar(i, false),
                '`' => i = self.backquote(i + 1),
                c => {
                    self.push(c);
                    i += 1;
                }
            }
        }
        if stop {
            self.lost = true;
        }
        i
    }

    /// A redirection operator at `i` (`<`, `>`, `>>`, `<<`, `<<<`, `>&`, `<>`, `>|`, `&>`...);
    /// returns the position after it. Its target word is dropped.
    fn redirection(&mut self, mut i: usize) -> usize {
        if self.at(i) == Some('<') && self.at(i + 1) == Some('<') {
            if self.at(i + 2) == Some('<') {
                self.target = true;
                self.herestring = true;
                return i + 3;
            }
            i += 2;
            let strip = self.at(i) == Some('-');
            if strip {
                i += 1;
            }
            while matches!(self.at(i), Some(' ' | '\t')) {
                i += 1;
            }
            // The delimiter: one word, quote removal only.
            let mut delim = String::new();
            let mut quoted = false;
            while let Some(c) = self.at(i) {
                match c {
                    '\'' | '"' => {
                        quoted = true;
                        let q = c;
                        i += 1;
                        while let Some(d) = self.at(i) {
                            if d == q {
                                break;
                            }
                            delim.push(d);
                            i += 1;
                        }
                        i += 1;
                    }
                    '\\' => {
                        quoted = true;
                        if let Some(d) = self.at(i + 1) {
                            delim.push(d);
                        }
                        i += 2;
                    }
                    c if c.is_whitespace() || ";&|()<>".contains(c) => break,
                    c => {
                        delim.push(c);
                        i += 1;
                    }
                }
            }
            if delim.is_empty() {
                self.lost = true;
            }
            self.pending.push((delim, strip, quoted, self.cmds.len()));
            return i;
        }
        // `<`, `>`, `>>`, `>|`, `<>`, `<&`, `>&`.
        let out = self.at(i) == Some('>') || self.at(i + 1) == Some('>');
        i += 1;
        if matches!(self.at(i), Some('>' | '|'))
            || (self.at(i - 1) == Some('<') && self.at(i) == Some('>'))
        {
            i += 1;
        }
        if self.at(i) == Some('&') {
            i += 1;
            let start = i;
            while self.at(i).is_some_and(|c| c.is_ascii_digit() || c == '-') {
                i += 1;
            }
            if i > start {
                // A descriptor duplicated or closed: no file.
                return i;
            }
        }
        if out {
            self.cur.writes = true;
        }
        self.target = true;
        i
    }

    /// After a newline at `i`: the bodies of the here-documents opened on the line that ended.
    fn heredoc_bodies(&mut self, mut i: usize) -> usize {
        for (delim, strip, quoted, cmd) in std::mem::take(&mut self.pending) {
            let mut body = String::new();
            let mut found = false;
            while i < self.c.len() {
                let end = (i..self.c.len())
                    .find(|&j| self.c[j] == '\n')
                    .unwrap_or(self.c.len());
                let line: String = self.c[i..end].iter().collect();
                i = end + 1;
                let cmp = if strip {
                    line.trim_start_matches('\t')
                } else {
                    &line
                };
                if cmp == delim {
                    found = true;
                    break;
                }
                body.push_str(&line);
                body.push('\n');
            }
            if !found {
                self.lost = true;
                i = self.c.len();
            }
            let heredoc = Heredoc { quoted, body };
            match self.cmds.get_mut(cmd) {
                Some(c) => c.heredocs.push(heredoc),
                None => self.cur.heredocs.push(heredoc),
            }
        }
        i.min(self.c.len())
    }

    /// `"..."` from `i` (after the quote); returns the position after the closing quote.
    fn double_quoted(&mut self, mut i: usize) -> usize {
        self.word.get_or_insert_with(Word::default);
        while let Some(c) = self.at(i) {
            match c {
                '"' => return i + 1,
                '\\' => {
                    match self.at(i + 1) {
                        Some('\n') => {}
                        Some(n @ ('$' | '`' | '"' | '\\')) => self.push(n),
                        Some(n) => {
                            self.push('\\');
                            self.push(n);
                        }
                        None => self.push('\\'),
                    }
                    i += 2;
                }
                '$' => i = self.dollar(i, true),
                '`' => i = self.backquote(i + 1),
                c => {
                    self.push(c);
                    i += 1;
                }
            }
        }
        self.lost = true;
        i
    }

    /// A substitution body from `i` to its `)`; returns the position after it.
    fn substitution(&mut self, i: usize) -> usize {
        let mut inner = Lexer::new(self.c, self.vars);
        let end = inner.run(i, true);
        if inner.lost {
            self.lost = true;
        }
        let end = end.min(self.c.len());
        self.cur.substs.push(self.c[i..end].iter().collect());
        end + 1
    }

    /// A backquoted substitution from `i` (after the backquote).
    fn backquote(&mut self, i: usize) -> usize {
        let mut j = i;
        while let Some(c) = self.at(j) {
            match c {
                '\\' => j += 2,
                '`' => break,
                _ => j += 1,
            }
        }
        if self.at(j).is_none() {
            self.lost = true;
        }
        let end = j.min(self.c.len());
        let body: String = self.c[i..end].iter().collect();
        self.cur.substs.push(body.clone());
        self.push('`');
        self.push_str(&body);
        self.push('`');
        if let Some(w) = self.word.as_mut() {
            w.built = true;
        }
        end + 1
    }

    /// `$` at `i`: a substitution, an arithmetic expansion, a variable or a `$'...'` quote.
    fn dollar(&mut self, i: usize, quoted: bool) -> usize {
        match self.at(i + 1) {
            Some('(') if self.at(i + 2) == Some('(') => {
                // Arithmetic: kept as text up to its `))`.
                let mut j = i + 3;
                let mut depth = 0usize;
                while let Some(c) = self.at(j) {
                    if c == '(' {
                        depth += 1;
                    } else if c == ')' {
                        if depth == 0 && self.at(j + 1) == Some(')') {
                            break;
                        }
                        depth = depth.saturating_sub(1);
                    }
                    j += 1;
                }
                if self.at(j).is_none() {
                    self.lost = true;
                }
                let end = (j + 2).min(self.c.len());
                // The substitutions inside the expression run.
                let inner: String = self.c[(i + 3).min(end)..j.min(self.c.len())]
                    .iter()
                    .collect();
                match substitutions(&inner) {
                    Some(bodies) => self.cur.substs.extend(bodies),
                    None => self.lost = true,
                }
                let text: String = self.c[i..end].iter().collect();
                self.push_str(&text);
                if let Some(w) = self.word.as_mut() {
                    w.built = true;
                }
                end
            }
            Some('(') => {
                let start = i + 2;
                let end = self.substitution(start);
                let text: String = self.c[i..end.min(self.c.len())].iter().collect();
                self.push_str(&text);
                if let Some(w) = self.word.as_mut() {
                    w.built = true;
                }
                end
            }
            Some('\'') if !quoted => {
                // `$'...'`: its text, with a backslash keeping the next character.
                self.word.get_or_insert_with(Word::default);
                let mut j = i + 2;
                while let Some(c) = self.at(j) {
                    match c {
                        '\'' => break,
                        '\\' => {
                            if let Some(n) = self.at(j + 1) {
                                self.push(n);
                            }
                            j += 2;
                        }
                        c => {
                            self.push(c);
                            j += 1;
                        }
                    }
                }
                if self.at(j).is_none() {
                    self.lost = true;
                }
                j + 1
            }
            Some('{') => {
                let Some(end) = self.brace_end(i + 2) else {
                    self.lost = true;
                    self.push('$');
                    return i + 1;
                };
                let inner: String = self.c[i + 2..end].iter().collect();
                let written = format!("${{{inner}}}");
                if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    self.variable(&inner, &written, quoted);
                } else {
                    // An expansion with an operator (`${v:-word}`): its value is not known, and
                    // the substitutions in its word run.
                    match substitutions(&inner) {
                        Some(bodies) => self.cur.substs.extend(bodies),
                        None => self.lost = true,
                    }
                    self.push_str(&written);
                    if let Some(w) = self.word.as_mut() {
                        w.built = true;
                    }
                }
                end + 1
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let mut j = i + 1;
                while self
                    .at(j)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    j += 1;
                }
                let name: String = self.c[i + 1..j].iter().collect();
                let written = format!("${name}");
                self.variable(&name, &written, quoted);
                j
            }
            _ => {
                self.push('$');
                i + 1
            }
        }
    }

    /// The position of the `}` that closes a `${` whose text starts at `i`: nested `${...}`,
    /// quotes and substitutions are passed over.
    fn brace_end(&self, mut i: usize) -> Option<usize> {
        let mut depth = 0usize;
        while let Some(c) = self.at(i) {
            match c {
                '\\' => i += 2,
                '\'' => i = (i + 1..self.c.len()).find(|&j| self.c[j] == '\'')? + 1,
                '"' => {
                    let mut j = i + 1;
                    while self.at(j)? != '"' {
                        j += if self.at(j) == Some('\\') { 2 } else { 1 };
                    }
                    i = j + 1;
                }
                '$' if self.at(i + 1) == Some('(') => {
                    let mut inner = Lexer::new(self.c, self.vars);
                    let end = inner.run(i + 2, true);
                    if inner.lost {
                        return None;
                    }
                    i = end + 1;
                }
                '$' if self.at(i + 1) == Some('{') => {
                    depth += 1;
                    i += 2;
                }
                '}' if depth == 0 => return Some(i),
                '}' => {
                    depth -= 1;
                    i += 1;
                }
                _ => i += 1,
            }
        }
        None
    }

    /// The value of variable `name`, or the text as `written` when it is not known.
    fn variable(&mut self, name: &str, written: &str, quoted: bool) {
        let Some(value) = self.vars.get(name) else {
            self.push_str(written);
            return;
        };
        if quoted {
            self.push_str(value);
            return;
        }
        // Unquoted: the value splits at blanks into words; it is not read again as syntax.
        let value = value.clone();
        let mut first = true;
        if value.starts_with(char::is_whitespace) {
            self.end_word();
        }
        for field in value.split_whitespace() {
            if !first {
                self.end_word();
            }
            self.push_str(field);
            first = false;
        }
        if value.ends_with(char::is_whitespace) {
            self.end_word();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<Vec<String>> {
        parse(text, &Vars::new())
            .cmds
            .into_iter()
            .map(|c| c.words.into_iter().map(|w| w.text).collect())
            .collect()
    }

    #[test]
    fn quotes_keep_a_word_whole_and_redirections_are_dropped() {
        assert_eq!(
            words("git commit -m \"bash -n\" 2>&1 >out -- a"),
            [["git", "commit", "-m", "bash -n", "--", "a"]]
        );
        assert_eq!(words("git >/dev/null commit"), [["git", "commit"]]);
        assert_eq!(
            words("a 'b c'; d | e && f"),
            [vec!["a", "b c"], vec!["d"], vec!["e"], vec!["f"]]
        );
    }

    #[test]
    fn heredoc_bodies_and_substitutions_are_kept_apart() {
        let p = parse(
            "cat <<'EOF' | x\ngit commit -n\nEOF\necho \"$(git status)\"",
            &Vars::new(),
        );
        assert!(!p.lost);
        assert_eq!(p.cmds[0].heredocs[0].body, "git commit -n\n");
        assert!(p.cmds[0].heredocs[0].quoted);
        assert!(p.cmds[0].piped);
        assert_eq!(p.cmds[2].substs, ["git status"]);
        let p = parse("x \"$(cat <<'EOF'\na ) b\nEOF\n)\" y", &Vars::new());
        assert!(!p.lost, "{p:?}");
        assert_eq!(p.cmds.last().unwrap().words.last().unwrap().text, "y");
    }

    #[test]
    fn a_here_string_is_input_and_an_expansions_substitution_is_kept() {
        let p = parse("bash <<< 'git commit -n' x", &Vars::new());
        assert_eq!(p.cmds[0].heredocs[0].body, "git commit -n");
        assert_eq!(p.cmds[0].words.len(), 2);
        let p = parse("echo \"${v:-$(git status)}\" ${w}", &Vars::new());
        assert!(!p.lost);
        assert_eq!(p.cmds[0].substs, ["git status"]);
        assert!(p.cmds[0].words[1].built);
        assert!(!p.cmds[0].words[2].built);
    }

    #[test]
    fn a_value_is_split_but_not_read_again() {
        let mut vars = Vars::new();
        vars.insert("M".into(), "'".into());
        vars.insert("F".into(), "-a -n".into());
        let p = parse("git commit -m $M -n $F \"$F\"", &vars);
        let w: Vec<&str> = p.cmds[0].words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(w, ["git", "commit", "-m", "'", "-n", "-a", "-n", "-a -n"]);
    }
}
