extern crate std;
use super::*;
use alloc::string::ToString;

fn p(s: &str) -> Url {
    Url::parse(s).unwrap_or_else(|e| panic!("parse {:?}: {:?}", s, e))
}

#[test]
fn basic_components() {
    let u = p("HTTP://User:Pw@Example.COM:8080/a/b?x=1&y=2#frag");
    assert_eq!(u.scheme, "http");
    assert_eq!(u.userinfo, "User:Pw");
    assert_eq!(u.host_str(), "example.com");
    assert_eq!(u.port, Some(8080));
    assert_eq!(u.path, "/a/b");
    assert_eq!(u.query.as_deref(), Some("x=1&y=2"));
    assert_eq!(u.fragment.as_deref(), Some("frag"));
    assert_eq!(u.request_target(), "/a/b?x=1&y=2");
    assert_eq!(u.authority(), "example.com:8080");
    assert_eq!(u.credentials(), Some(("User".into(), "Pw".into())));
    assert_eq!(
        u.to_string(),
        "http://User:Pw@example.com:8080/a/b?x=1&y=2#frag"
    );
}

#[test]
fn defaults_and_normalisation() {
    assert_eq!(p("http://example.com").to_string(), "http://example.com/");
    assert_eq!(p("https://example.com:443/x").port, None);
    assert_eq!(p("https://example.com:443/x").port_or_default(), Some(443));
    assert_eq!(p("http://example.com:/x").port, None);
    assert_eq!(p("http://a/b/c/./../../g").path, "/g");
    assert_eq!(
        p("  http://a/b c?d e#f g \n").to_string(),
        "http://a/b%20c?d%20e#f%20g"
    );
    assert_eq!(p("http:\\\\a\\b\\c").to_string(), "http://a/b/c");
    assert_eq!(p("http:example.com/x").to_string(), "http://example.com/x");
    assert_eq!(p("http://a/%7Efoo/%2e%2E/bar").path, "/bar");
    assert_eq!(p("http://a/ü").path, "/%C3%BC");
}

#[test]
fn hosts() {
    let u = p("http://[2001:DB8::1]:8080/");
    assert_eq!(u.host_str(), "2001:db8::1");
    assert_eq!(u.authority(), "[2001:db8::1]:8080");
    assert_eq!(p("http://[::1]/").to_string(), "http://[::1]/");
    assert_eq!(
        p("http://bücher.example/").host_str(),
        "xn--bcher-kva.example"
    );
    assert_eq!(p("http://MÜNCHEN.de/").host_str(), "xn--mnchen-3ya.de");
    assert_eq!(p("http://%65xample.com/").host_str(), "example.com");
    assert_eq!(Url::parse("http:///x"), Err(Error::MissingHost));
    assert_eq!(Url::parse("http://a:99999/"), Err(Error::InvalidPort));
    assert_eq!(Url::parse("http://a:x/"), Err(Error::InvalidPort));
    assert_eq!(Url::parse("http://a b/"), Err(Error::InvalidHost));
    assert_eq!(Url::parse("/relative"), Err(Error::RelativeWithoutBase));
}

#[test]
fn punycode_vectors() {
    // RFC 3492 section 7.1 (lower-cased) and common IDNs.
    assert_eq!(punycode_encode("bücher").unwrap(), "bcher-kva");
    assert_eq!(punycode_encode("münchen").unwrap(), "mnchen-3ya");
    assert_eq!(
        punycode_encode("他们为什么不说中文").unwrap(),
        "ihqwcrb4cv8a8dqg056pqjye"
    );
    assert_eq!(punycode_encode("abc").unwrap(), "abc-");
}

#[test]
fn opaque_and_file() {
    let m = p("mailto:Someone@Example.com?subject=hi there");
    assert_eq!(m.host, None);
    assert_eq!(m.path, "Someone@Example.com");
    assert_eq!(m.query.as_deref(), Some("subject=hi%20there"));
    let d = p("data:text/plain,hello");
    assert_eq!(d.path, "text/plain,hello");
    let f = p("file:///etc/hosts");
    assert_eq!(f.host_str(), "");
    assert_eq!(f.path, "/etc/hosts");
    assert_eq!(f.to_string(), "file:///etc/hosts");
    assert_eq!(p("about:blank").to_string(), "about:blank");
}

#[test]
fn rfc3986_normal_examples() {
    let base = p("http://a/b/c/d;p?q");
    let cases = [
        ("g:h", "g:h"),
        ("g", "http://a/b/c/g"),
        ("./g", "http://a/b/c/g"),
        ("g/", "http://a/b/c/g/"),
        ("/g", "http://a/g"),
        ("//g", "http://g/"),
        ("?y", "http://a/b/c/d;p?y"),
        ("g?y", "http://a/b/c/g?y"),
        ("#s", "http://a/b/c/d;p?q#s"),
        ("g#s", "http://a/b/c/g#s"),
        ("g?y#s", "http://a/b/c/g?y#s"),
        (";x", "http://a/b/c/;x"),
        ("g;x", "http://a/b/c/g;x"),
        ("g;x?y#s", "http://a/b/c/g;x?y#s"),
        ("", "http://a/b/c/d;p?q"),
        (".", "http://a/b/c/"),
        ("./", "http://a/b/c/"),
        ("..", "http://a/b/"),
        ("../", "http://a/b/"),
        ("../g", "http://a/b/g"),
        ("../..", "http://a/"),
        ("../../", "http://a/"),
        ("../../g", "http://a/g"),
    ];
    for (r, want) in cases {
        assert_eq!(base.join(r).unwrap().to_string(), want, "reference {:?}", r);
    }
}

#[test]
fn rfc3986_abnormal_examples() {
    let base = p("http://a/b/c/d;p?q");
    let cases = [
        ("../../../g", "http://a/g"),
        ("../../../../g", "http://a/g"),
        ("/./g", "http://a/g"),
        ("/../g", "http://a/g"),
        ("g.", "http://a/b/c/g."),
        (".g", "http://a/b/c/.g"),
        ("g..", "http://a/b/c/g.."),
        ("..g", "http://a/b/c/..g"),
        ("./../g", "http://a/b/g"),
        ("./g/.", "http://a/b/c/g/"),
        ("g/./h", "http://a/b/c/g/h"),
        ("g/../h", "http://a/b/c/h"),
        ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
        ("g;x=1/../y", "http://a/b/c/y"),
        ("g?y/./x", "http://a/b/c/g?y/./x"),
        ("g?y/../x", "http://a/b/c/g?y/../x"),
        ("g#s/./x", "http://a/b/c/g#s/./x"),
        ("g#s/../x", "http://a/b/c/g#s/../x"),
        // Browsers (WHATWG) treat a same-scheme reference as relative.
        ("http:g", "http://a/b/c/g"),
    ];
    for (r, want) in cases {
        assert_eq!(base.join(r).unwrap().to_string(), want, "reference {:?}", r);
    }
}

#[test]
fn browser_style_references() {
    let base = p("https://portal.example.net/login/index.php?next=%2F");
    assert_eq!(
        base.join("  ../auth.php?user=a b ").unwrap().to_string(),
        "https://portal.example.net/auth.php?user=a%20b"
    );
    assert_eq!(
        base.join("\\\\cdn.example.org\\x.css").unwrap().to_string(),
        "https://cdn.example.org/x.css"
    );
    assert_eq!(
        base.join("//other:8443/").unwrap().to_string(),
        "https://other:8443/"
    );
    assert_eq!(
        base.join("HTTP://Elsewhere/").unwrap().to_string(),
        "http://elsewhere/"
    );
    assert_eq!(base.join("#top").unwrap().fragment.as_deref(), Some("top"));
    let mail = p("mailto:a@b");
    assert!(mail.join("x").is_err());
    assert_eq!(mail.join("#f").unwrap().to_string(), "mailto:a@b#f");
}

#[test]
fn address_bar_input() {
    assert_eq!(
        Url::from_user_input("example.com").unwrap().to_string(),
        "http://example.com/"
    );
    assert_eq!(
        Url::from_user_input("localhost:8080/x")
            .unwrap()
            .to_string(),
        "http://localhost:8080/x"
    );
    assert_eq!(
        Url::from_user_input("https://a/").unwrap().to_string(),
        "https://a/"
    );
    assert_eq!(
        Url::from_user_input("/etc/hosts").unwrap().to_string(),
        "file:///etc/hosts"
    );
    assert_eq!(
        Url::from_user_input("about:blank").unwrap().to_string(),
        "about:blank"
    );
}

#[test]
fn percent_and_form() {
    assert_eq!(percent_decode_str("a%20b%zz%4"), "a b%zz%4");
    assert_eq!(form::encode("a b&c=d/é"), "a+b%26c%3Dd%2F%C3%A9");
    assert_eq!(
        form::serialize([("user", "joe"), ("pass", "p@ss word")]),
        "user=joe&pass=p%40ss+word"
    );
    assert_eq!(
        form::parse("a=1&b=x+y&c=%26&d"),
        alloc::vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "x y".to_string()),
            ("c".to_string(), "&".to_string()),
            ("d".to_string(), String::new()),
        ]
    );
    assert_eq!(p("http://a/x%20y.pdf").file_name(), "x y.pdf");
}
