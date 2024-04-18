use std::{net::Ipv4Addr, str::FromStr};

use http::{Request, StatusCode};

use nbio::{http::HttpClient, ReadStatus, Session};

#[test]
fn test_google_chunked_response() {
    // create the client and make the request
    let mut client = HttpClient::new();
    let mut conn = client
        .request(Request::get("https://www.google.com").body(()).unwrap())
        .unwrap();

    // read the conn until a full response is received
    loop {
        conn.drive().unwrap();
        if let ReadStatus::Data(r) = conn.read().unwrap() {
            // validate the response
            assert_eq!(r.status(), StatusCode::OK);
            assert!(String::from_utf8_lossy(r.body()).ends_with("</html>"));
            break;
        }
    }
}

#[test]
fn test_simple_response() {
    // create the client and make the request
    let mut client = HttpClient::new();
    let mut conn = client
        .request(Request::get("http://icanhazip.com").body(()).unwrap())
        .unwrap();

    // read the conn until a full response is received
    loop {
        conn.drive().unwrap();
        if let ReadStatus::Data(r) = conn.read().unwrap() {
            // validate the response
            assert_eq!(r.status(), StatusCode::OK);
            let body = String::from_utf8_lossy(r.body());
            Ipv4Addr::from_str(body.trim()).expect("IP V4 address as body");
            break;
        }
    }
}

#[test]
fn test_keep_alive() {
    // create the client and make the initial request
    let mut client = HttpClient::new();
    let mut conn = client
        .request(Request::get("http://icanhazip.com").body(()).unwrap())
        .unwrap();

    // read the conn until the first full response is received
    loop {
        conn.drive().unwrap();
        if let ReadStatus::Data(r) = conn.read().unwrap() {
            // validate the response
            assert_eq!(r.status(), StatusCode::OK);
            let body = String::from_utf8_lossy(r.body());
            println!("body: {body:?}");
            Ipv4Addr::from_str(body.trim()).expect("IP V4 address as body");
            break;
        }
    }

    // write another request
    conn.write(
        &Request::get("http://icanhazip.com")
            .body(Vec::new())
            .unwrap(),
    )
    .unwrap();

    // read the conn until the second full response is received
    loop {
        conn.drive().unwrap();
        if let ReadStatus::Data(r) = conn.read().unwrap() {
            // validate the response
            assert_eq!(r.status(), StatusCode::OK);
            let body = String::from_utf8_lossy(r.body());
            println!("body: {body:?}");
            Ipv4Addr::from_str(body.trim()).expect("IP V4 address as body");
            break;
        }
    }
}
