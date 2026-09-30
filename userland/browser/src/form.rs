//! Form submission: build the request for a form from the current field
//! values (HTML "constructing the entry list").

use rustos_rt::prelude::*;
use layout::{Field, FieldKind, Form};
use webclient::httpc::multipart::{self, Part};
use webclient::httpc::{form as urlenc, Request, Url};

/// Build the request for submitting form `fi` with button `submitter`.
pub fn submission(forms: &[Form], fields: &[Field], fi: usize, submitter: Option<usize>, base: &Url) -> Result<Request, String> {
    let form = forms.get(fi).ok_or("no such form")?;
    let sub = submitter.map(|i| &fields[i]);
    let action = sub.and_then(|s| s.formaction.clone()).unwrap_or_else(|| form.action.clone());
    let method = sub.and_then(|s| s.formmethod.clone()).unwrap_or_else(|| form.method.clone());
    let enctype = sub.and_then(|s| s.formenctype.clone()).unwrap_or_else(|| form.enctype.clone());
    let url = if action.is_empty() { base.without_fragment() } else { base.join(&action).map_err(|e| e.to_string())? };

    let mut entries: Vec<(String, String)> = Vec::new();
    let mut files: Vec<(String, String)> = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        if f.form != Some(fi) || f.disabled {
            continue;
        }
        let is_button = matches!(f.kind, FieldKind::Submit | FieldKind::Image | FieldKind::Reset | FieldKind::Button);
        if is_button && Some(i) != submitter {
            continue;
        }
        match f.kind {
            FieldKind::Image => {
                // Clicked at (0, 0).
                let prefix = if f.name.is_empty() { String::new() } else { format!("{}.", f.name) };
                entries.push((format!("{}x", prefix), String::from("0")));
                entries.push((format!("{}y", prefix), String::from("0")));
                continue;
            }
            FieldKind::Reset | FieldKind::Button => continue,
            _ => {}
        }
        if f.name.is_empty() {
            continue;
        }
        match f.kind {
            FieldKind::Checkbox | FieldKind::Radio if !f.checked => {}
            FieldKind::Select => {
                if let Some(o) = f.options.get(f.selected) {
                    entries.push((f.name.clone(), o.value.clone()));
                }
            }
            FieldKind::Textarea => entries.push((f.name.clone(), f.value.replace("\r\n", "\n").replace('\n', "\r\n"))),
            FieldKind::File => files.push((f.name.clone(), f.value.clone())),
            _ => entries.push((f.name.clone(), f.value.clone())),
        }
    }

    let files: Vec<(String, String, String, Vec<u8>)> = files
        .iter()
        .map(|(name, path)| {
            let data = if path.is_empty() { Vec::new() } else { rustos_rt::fs::read(path).unwrap_or_default() };
            let fname = path.rsplit('/').next().unwrap_or("").to_string();
            (name.clone(), fname, String::from("application/octet-stream"), data)
        })
        .collect();
    build_request(url, &method, &enctype, &entries, &files)
}

/// Encode a form data set (`files`: name, file name, type, contents) as a
/// GET or POST request to `url`.
pub fn build_request(
    mut url: Url,
    method: &str,
    enctype: &str,
    entries: &[(String, String)],
    files: &[(String, String, String, Vec<u8>)],
) -> Result<Request, String> {
    url.fragment = None;
    if !method.eq_ignore_ascii_case("post") {
        let mut all: Vec<(&str, &str)> = entries.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        all.extend(files.iter().map(|f| (f.0.as_str(), f.1.as_str())));
        url.query = Some(urlenc::serialize(all.into_iter()));
        return Ok(Request::get(url));
    }
    match enctype {
        "multipart/form-data" => {
            let mut parts: Vec<(&str, Part)> = entries.iter().map(|(k, v)| (k.as_str(), Part::Text(v))).collect();
            for (name, fname, ty, data) in files {
                let ty = if ty.is_empty() { "application/octet-stream" } else { ty.as_str() };
                parts.push((name.as_str(), Part::File { filename: fname, content_type: ty, data }));
            }
            let (ct, body) = multipart::encode(&parts, rustos_rt::time::micros());
            Ok(Request::post(url, &ct, body))
        }
        "text/plain" => {
            let mut body = String::new();
            for (k, v) in entries {
                body.push_str(&format!("{}={}\r\n", k, v));
            }
            Ok(Request::post(url, "text/plain", body.into_bytes()))
        }
        _ => {
            let mut all: Vec<(&str, &str)> = entries.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            all.extend(files.iter().map(|f| (f.0.as_str(), f.1.as_str())));
            let body = urlenc::serialize(all.into_iter());
            Ok(Request::post(url, "application/x-www-form-urlencoded", body.into_bytes()))
        }
    }
}
