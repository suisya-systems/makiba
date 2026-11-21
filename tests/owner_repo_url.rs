//! Issue 198: the parser that tells which GitHub repository an `origin` URL names. User info is
//! dropped only inside the authority, so an `@` elsewhere cannot move the host to github.com.

use makiba::daemon::owner_repo_from_url as f;

#[test]
fn every_protocol_names_the_repository() {
    for u in [
        "https://github.com/O/R",
        "https://github.com/o/r.git",
        "http://github.com/o/r/",
        "https://user@github.com/o/r.git",
        "git@github.com:o/r.git",
        "git@github.com:o/r",
        "ssh://git@github.com/o/r",
        "git://github.com/o/r.git",
    ] {
        assert_eq!(f(u).as_deref(), Some("o/r"), "{u}");
    }
}

#[test]
fn an_at_sign_outside_the_authority_does_not_make_github_the_host() {
    for u in [
        "https://example.org/@github.com/o/r.git",
        "https://example.org/x@github.com/o/r",
        "ssh://git@example.org/@github.com:o/r",
        "/srv/x@github.com:o/r",
        "./x@github.com:o/r",
        "file://x@github.com/o/r",
        "https://github.com@example.org/o/r",
        "git@example.org:o/r.git",
    ] {
        assert_eq!(f(u), None, "{u}");
    }
}
