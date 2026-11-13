use std::borrow::Cow;
use tauri::http::{HeaderValue, Method, Request, Response, StatusCode, header};

pub fn serve_video_range(request: Request<Vec<u8>>, response: &mut Response<Cow<'static, [u8]>>) {
    let uri = request.uri();
    if uri.scheme_str() != Some("tauri")
        || uri.host() != Some("localhost")
        || uri.path() != "/media/nevio-mcfly.mp4"
        || response.status() != StatusCode::OK
        || response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            != Some("video/mp4")
    {
        return;
    }

    let len = response.body().len();
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if request.method() == Method::HEAD {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
        *response.body_mut() = Cow::Borrowed(&[]);
        return;
    }
    if request.method() != Method::GET {
        return;
    }

    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    match range.and_then(|value| parse_range(value, len)) {
        Some(Ok((start, end))) => {
            let bytes = response.body_mut().to_mut();
            bytes.copy_within(start..=end, 0);
            bytes.truncate(end - start + 1);
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")).unwrap(),
            );
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(end - start + 1));
        }
        Some(Err(())) => {
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            *response.body_mut() = Cow::Borrowed(&[]);
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{len}")).unwrap(),
            );
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(0));
        }
        None => {
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
        }
    }
}

// WebKit uses one byte interval at a time. Other units or multipart requests receive the full asset.
fn parse_range(value: &str, len: usize) -> Option<Result<(usize, usize), ()>> {
    let (unit, value) = value.split_once('=')?;
    if !unit.eq_ignore_ascii_case("bytes") || value.contains(',') {
        return None;
    }
    let (first, last) = value.trim().split_once('-')?;
    if first.is_empty() {
        let suffix = last.parse::<usize>().ok()?;
        if suffix == 0 || len == 0 {
            return Some(Err(()));
        }
        return Some(Ok((len.saturating_sub(suffix), len - 1)));
    }

    let start = first.parse::<usize>().ok()?;
    let end = if last.is_empty() {
        len.saturating_sub(1)
    } else {
        last.parse::<usize>().ok()?
    };
    if start >= len || start > end {
        return Some(Err(()));
    }
    Some(Ok((start, end.min(len - 1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(method: Method, path: &str, range: &str) -> Response<Cow<'static, [u8]>> {
        let request = Request::builder()
            .method(method)
            .uri(format!("tauri://localhost/{path}"))
            .header(header::RANGE, range)
            .body(vec![])
            .unwrap();
        let mut response = Response::builder()
            .header(header::CONTENT_TYPE, "video/mp4")
            .body(Cow::Borrowed(&b"0123456789"[..]))
            .unwrap();
        serve_video_range(request, &mut response);
        response
    }

    #[test]
    fn video_respects_webkit_ranges_and_leaves_other_assets_untouched() {
        let path = "media/nevio-mcfly.mp4";
        let response = resource(Method::GET, path, "bytes=0-1");
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 0-1/10");
        assert_eq!(response.body().as_ref(), b"01");
        assert_eq!(
            resource(Method::GET, path, "bytes=8-").body().as_ref(),
            b"89"
        );
        assert_eq!(
            resource(Method::GET, path, "bytes=-3").body().as_ref(),
            b"789"
        );
        let response = resource(Method::GET, path, "bytes=10-");
        assert_eq!(response.status(), 416);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */10");
        let response = resource(Method::HEAD, path, "bytes=0-1");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
        assert!(response.body().is_empty());
        let response = resource(Method::GET, "different.mp4", "bytes=0-1");
        assert_eq!(response.status(), 200);
        assert!(!response.headers().contains_key(header::ACCEPT_RANGES));
    }
}
