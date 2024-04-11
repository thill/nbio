use std::{net::Ipv4Addr, str::FromStr};

use http::{Request, StatusCode};
use tcp_stream::OwnedTLSConfig;

use nbio::{http::HttpClient, ReadStatus, Session};

#[test]
fn test_google_chunked_response() {
    // create the client and make the request
    let mut client = HttpClient::new(OwnedTLSConfig::default());
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
    let mut client = HttpClient::new(OwnedTLSConfig::default());
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
