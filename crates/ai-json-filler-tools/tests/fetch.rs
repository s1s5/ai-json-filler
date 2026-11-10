use ai_json_filler_tools::fetch::{slice_content, validate_url};

#[test]
fn rejects_loopback() {
    assert!(validate_url("http://127.0.0.1/", false).is_some());
}

#[test]
fn rejects_private() {
    assert!(validate_url("http://10.0.0.1/", false).is_some());
    assert!(validate_url("http://192.168.1.1/", false).is_some());
    assert!(validate_url("http://100.64.0.1/", false).is_some());
}

#[test]
fn rejects_metadata_ip() {
    assert!(validate_url("http://169.254.169.254/", false).is_some());
    assert!(validate_url("http://[::ffff:169.254.169.254]/", false).is_some());
}

#[test]
fn rejects_localhost_with_dot() {
    assert!(validate_url("http://localhost.:8080/", false).is_some());
}

#[test]
fn allows_public() {
    assert!(validate_url("https://example.com/", false).is_none());
}

#[test]
fn slice_returns_next_offset() {
    let content = "0123456789";
    let (r1, t1, off1) = slice_content(content, 0, 4);
    assert_eq!(r1, "0123");
    assert!(t1);
    assert_eq!(off1, Some(4));
    let (r2, t2, off2) = slice_content(content, 4, 4);
    assert_eq!(r2, "4567");
    assert!(t2);
    assert_eq!(off2, Some(8));
    let (r3, t3, off3) = slice_content(content, 8, 4);
    assert_eq!(r3, "89");
    assert!(!t3);
    assert_eq!(off3, None);
}
