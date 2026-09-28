//! Hosted sign-in / registration pages. Server-rendered, script-free (the CSP
//! forbids scripts entirely), every dynamic value HTML-escaped. Each form
//! carries the pending authorization request id and a synchronizer CSRF token.

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

const MARK: &str = r##"<svg viewBox="0 0 32 32" aria-hidden="true"><rect x="2" y="9" width="11" height="14" rx="2" fill="currentColor"/><rect x="19" y="9" width="11" height="14" rx="5.5" fill="currentColor"/><circle cx="16" cy="16" r="6" fill="#b58a34"/></svg>"##;

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{t} · ChainDeal</title><link rel="stylesheet" href="/oauth/assets/auth.css"></head>
<body><main><div class="brand">{MARK}<span>ChainDeal</span></div>{body}
<p class="fine">Secured by ChainDeal Auth · OAuth 2.1 with PKCE</p></main></body></html>"#,
        t = esc(title)
    )
}

fn alerts(error: Option<&str>, notice: Option<&str>) -> String {
    let mut s = String::new();
    if let Some(e) = error {
        s.push_str(&format!(r#"<div class="err" role="alert">{}</div>"#, esc(e)));
    }
    if let Some(n) = notice {
        s.push_str(&format!(r#"<div class="note" role="status">{}</div>"#, esc(n)));
    }
    s
}

pub fn login(req: &str, csrf: &str, client: &str, email: &str, error: Option<&str>, notice: Option<&str>) -> String {
    page(
        "Sign in",
        &format!(
            r#"<div class="card"><h1>Sign in</h1><p class="sub">to continue to {client}</p>{alerts}
<form method="post" action="/oauth/login" autocomplete="on">
<input type="hidden" name="req" value="{req}"><input type="hidden" name="csrf" value="{csrf}">
<label for="email">Email</label><input id="email" name="email" type="email" autocomplete="username" required value="{email}" autofocus>
<label for="password">Password</label><input id="password" name="password" type="password" autocomplete="current-password" required>
<button type="submit">Sign in</button></form>
<p class="alt">New here? <a href="/oauth/register?req={req}">Create an account</a></p></div>"#,
            client = esc(client),
            alerts = alerts(error, notice),
            req = esc(req),
            csrf = esc(csrf),
            email = esc(email),
        ),
    )
}

pub fn register(req: &str, csrf: &str, email: &str, name: &str, error: Option<&str>) -> String {
    page(
        "Create account",
        &format!(
            r#"<div class="card"><h1>Create account</h1><p class="sub">Your email is stored encrypted.</p>{alerts}
<form method="post" action="/oauth/register">
<input type="hidden" name="req" value="{req}"><input type="hidden" name="csrf" value="{csrf}">
<label for="name">Display name</label><input id="name" name="name" autocomplete="name" required maxlength="64" value="{name}">
<label for="email">Email</label><input id="email" name="email" type="email" autocomplete="email" required value="{email}">
<label for="password">Password</label><input id="password" name="password" type="password" autocomplete="new-password" required minlength="12">
<p class="hint">At least 12 characters. Long passphrases are best.</p>
<label for="password2">Confirm password</label><input id="password2" name="password2" type="password" autocomplete="new-password" required minlength="12">
<button type="submit">Create account</button></form>
<p class="alt">Already registered? <a href="/oauth/authorize/resume?req={req}">Sign in</a></p></div>"#,
            alerts = alerts(error, None),
            req = esc(req),
            csrf = esc(csrf),
            email = esc(email),
            name = esc(name),
        ),
    )
}

pub fn error(title: &str, message: &str) -> String {
    page(title, &format!(r#"<div class="card"><h1>{}</h1><p class="sub">{}</p></div>"#, esc(title), esc(message)))
}
