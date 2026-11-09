//! Certificates: expiry dates of Let's Encrypt certificates (when readable), of every domain served
//! by the local nginx (probed with `openssl s_client` on 127.0.0.1:443) and of domains added by hand.
//! Certificates that expire within `WARN_DAYS` are highlighted and raised in the notification bell.

use leptos::prelude::*;

use crate::apps::files::shell_quote;
use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;

/// Expiry warning threshold (days), also used by the notification bell.
pub const WARN_DAYS: i64 = 15;

#[derive(Clone, Debug, PartialEq)]
pub struct Cert {
    /// "file" (Let's Encrypt live dir), "local" (nginx on this server), "remote" (added by hand).
    pub kind: String,
    pub name: String,
    /// Expiry as a Unix timestamp; None when the probe failed.
    pub expires: Option<i64>,
    pub issuer: String,
}

impl Cert {
    pub fn days_left(&self, now: i64) -> Option<i64> {
        self.expires.map(|e| (e - now).div_euclid(86_400))
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Report {
    pub certs: Vec<Cert>,
    pub needs_root: bool,
    pub no_openssl: bool,
}

/// Domains added by hand for a host (stored locally).
pub fn manual_domains(host: &str) -> Vec<String> {
    storage_get(&format!("sshos.certs.{host}")).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn set_manual_domains(host: &str, v: &[String]) {
    storage_set(&format!("sshos.certs.{host}"), &serde_json::to_string(v).unwrap_or_default());
}

/// `example.com` or `example.com:8443`.
pub fn valid_domain(d: &str) -> bool {
    let (name, port) = match d.rsplit_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (d, None),
    };
    name.contains('.')
        && name.len() <= 253
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !name.starts_with(['-', '.'])
        && port.is_none_or(|p| p.parse::<u16>().is_ok_and(|p| p > 0))
}

/// The probe script. Read-only: lists the certificates and connects to port 443 to read the chain.
pub fn script(manual: &[String]) -> String {
    let manual: Vec<String> = manual.iter().filter(|d| valid_domain(d)).map(|d| shell_quote(d)).collect();
    format!(
        r#"cert_line() {{ t=$(openssl x509 -noout -enddate -issuer 2>/dev/null) || {{ echo "@@cert $1|$2||"; return; }}
e=$(printf '%s\n' "$t" | sed -n 's/^notAfter=//p'); i=$(printf '%s\n' "$t" | sed -n 's/^issuer= *//p')
echo "@@cert $1|$2|$(date -d "$e" +%s 2>/dev/null)|$i"; }}
if ! command -v openssl >/dev/null 2>&1; then echo @@noopenssl; exit 0; fi
if [ -d /etc/letsencrypt/live ]; then
  if [ -x /etc/letsencrypt/live ] && [ -r /etc/letsencrypt/live ]; then
    for d in /etc/letsencrypt/live/*/; do [ -r "$d/cert.pem" ] && cert_line file "$(basename "$d")" < "$d/cert.pem"; done
  else echo @@needsroot; fi
fi
for d in $(grep -rhoE '^\s*server_name[^;]+' /etc/nginx/sites-enabled /etc/nginx/conf.d 2>/dev/null | sed 's/server_name//' | tr ' \t' '\n\n' | grep -E '^[A-Za-z0-9.-]+\.[A-Za-z]{{2,}}$' | sort -u | head -60); do
  echo | timeout 6 openssl s_client -connect 127.0.0.1:443 -servername "$d" 2>/dev/null | cert_line local "$d"
done
for d in {}; do h=${{d%%:*}}; p=${{d##*:}}; [ "$p" = "$d" ] && p=443
  echo | timeout 6 openssl s_client -connect "$h:$p" -servername "$h" 2>/dev/null | cert_line remote "$d"
done"#,
        manual.join(" ")
    )
}

pub fn parse(out: &str) -> Report {
    let mut r = Report::default();
    for l in out.lines() {
        match l.trim() {
            "@@needsroot" => r.needs_root = true,
            "@@noopenssl" => r.no_openssl = true,
            l => {
                let Some(rest) = l.strip_prefix("@@cert ") else { continue };
                let f: Vec<&str> = rest.splitn(4, '|').collect();
                if f.len() < 3 {
                    continue;
                }
                let issuer = f.get(3).map(|s| short_issuer(s)).unwrap_or_default();
                r.certs.push(Cert { kind: f[0].to_string(), name: f[1].to_string(), expires: f[2].trim().parse().ok(), issuer });
            }
        }
    }
    // A domain probed locally and also present as a file: keep both, but sort soonest first.
    r.certs.sort_by_key(|c| (c.expires.is_none(), c.expires.unwrap_or(i64::MAX), c.name.clone()));
    r
}

/// "C = US, O = Let's Encrypt, CN = R11" -> "Let's Encrypt R11".
fn short_issuer(s: &str) -> String {
    let get = |k: &str| s.split([',', '/']).map(str::trim).find_map(|p| p.strip_prefix(k).map(|v| v.trim_start_matches([' ', '=']).trim().to_string()));
    match (get("O"), get("CN")) {
        (Some(o), Some(cn)) if !cn.contains(&o) => format!("{o} {cn}"),
        (_, Some(cn)) => cn,
        (Some(o), None) => o,
        _ => s.trim().to_string(),
    }
}

pub fn now_secs() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

fn fmt_date(ts: i64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64 * 1000.0));
    format!("{:04}-{:02}-{:02}", d.get_full_year(), d.get_month() + 1, d.get_date())
}

#[component]
pub fn Certs(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let report = RwSignal::new(None::<Report>);
    let manual = RwSignal::new(manual_domains(&host.get_value()));
    let new_domain = RwSignal::new(String::new());
    let loading = RwSignal::new(false);
    let h = move || host.get_value();

    let load = move || {
        loading.set(true);
        kit::query(host.get_value(), script(&manual.get_untracked()), move |r| {
            // Expiring certificates are announced by the notification center (wm.rs), not here.
            let rep = parse(&r.stdout);
            let _ = loading.try_set(false);
            let _ = report.try_set(Some(rep));
        });
    };
    load();

    let add = move || {
        let d = new_domain.get_untracked().trim().trim_start_matches("https://").trim_end_matches('/').to_lowercase();
        if !valid_domain(&d) {
            ctx().error(tr("Domaine invalide (ex. exemple.fr ou exemple.fr:8443)", "Invalid domain (e.g. example.com or example.com:8443)"));
            return;
        }
        manual.update(|m| {
            if !m.contains(&d) {
                m.push(d)
            }
        });
        set_manual_domains(&h(), &manual.get_untracked());
        new_domain.set(String::new());
        load();
    };
    let remove = move |d: String| {
        manual.update(|m| m.retain(|x| *x != d));
        set_manual_domains(&h(), &manual.get_untracked());
        report.update(|r| {
            if let Some(r) = r {
                r.certs.retain(|c| !(c.kind == "remote" && c.name == d))
            }
        });
    };

    let body = move || {
        let Some(r) = report.get() else { return kit::loading_view().into_any() };
        if r.no_openssl {
            return kit::empty_view(tr("openssl n'est pas installé sur le serveur.", "openssl is not installed on the server.")).into_any();
        }
        let now = now_secs();
        let root_banner = r.needs_root.then(|| {
            view! {
                <div class="banner warn">
                    <Icon name="lock" size=15 />
                    <span class="grow">{tr("Les certificats Let's Encrypt (/etc/letsencrypt) ne sont lisibles qu'en root.", "Let's Encrypt certificates (/etc/letsencrypt) are only readable by root.")}</span>
                    <button class="small" on:click=move |_| kit::in_terminal(&h(), "certbot certificates")>{tr("Lister avec sudo…", "List with sudo…")}</button>
                </div>
            }
        });
        let table = if r.certs.is_empty() {
            kit::empty_view(tr("Aucun certificat trouvé. Ajoutez un domaine ci-dessous.", "No certificates found. Add a domain below.")).into_any()
        } else {
            view! {
                <div class="table-card">
                    <table class="table data-table">
                        <thead><tr><th>{tr("Domaine", "Domain")}</th><th>{tr("Source", "Source")}</th><th>{tr("Émetteur", "Issuer")}</th><th>{tr("Expire le", "Expires")}</th><th>{tr("Reste", "Left")}</th><th></th></tr></thead>
                        <tbody>
                            {r.certs
                                .into_iter()
                                .map(|c| {
                                    let days = c.days_left(now);
                                    let (cls, left) = match days {
                                        None => ("pill off", tr("injoignable", "unreachable").to_string()),
                                        Some(d) if d < 0 => ("pill crit", tr("expiré", "expired").to_string()),
                                        Some(d) if d <= WARN_DAYS => ("pill crit", format!("{d} j")),
                                        Some(d) if d <= 30 => ("pill warn", format!("{d} j")),
                                        Some(d) => ("pill ok", format!("{d} j")),
                                    };
                                    let src = match c.kind.as_str() {
                                        "file" => "Let's Encrypt",
                                        "local" => "nginx",
                                        _ => tr("ajouté", "added"),
                                    };
                                    let manual_row = c.kind == "remote";
                                    let n = c.name.clone();
                                    view! {
                                        <tr>
                                            <td class="name-cell"><div class="cell-flex"><Icon name="cert" size=14 /><span class="ellipsis">{c.name.clone()}</span></div></td>
                                            <td class="muted">{src}</td>
                                            <td class="muted small ellipsis">{c.issuer.clone()}</td>
                                            <td class="mono">{c.expires.map(fmt_date).unwrap_or_else(|| "—".into())}</td>
                                            <td><span class=cls>{left}</span></td>
                                            <td class="row-actions">
                                                {manual_row.then(|| view! {
                                                    <button class="icon-btn danger" title=tr("Retirer", "Remove") on:click=move |_| remove(n.clone())><Icon name="trash" size=14 /></button>
                                                })}
                                            </td>
                                        </tr>
                                    }
                                })
                                .collect_view()}
                        </tbody>
                    </table>
                </div>
            }
            .into_any()
        };
        view! { {root_banner} {table} }.into_any()
    };

    app_menus(vec![MenuSection::new(tr("Certificats", "Certificates"), vec![
        MenuItem::new(tr("Actualiser", "Refresh"), load),
        None,
        MenuItem::new(tr("Tester le renouvellement (dry-run)…", "Test renewal (dry-run)…"), move || kit::in_terminal(&h(), "certbot renew --dry-run")),
        MenuItem::new(tr("Renouveler les certificats…", "Renew certificates…"), move || kit::in_terminal(&h(), "certbot renew")),
        MenuItem::new(tr("Lister avec certbot…", "List with certbot…"), move || kit::in_terminal(&h(), "certbot certificates")),
    ])]);

    view! {
        <div class="split-app certs-app">
            <section class="main">
            <header class="main-head drag-region">
                <h2 class="grow ellipsis">{tr("Certificats TLS", "TLS certificates")}</h2>
                <button class="no-drag" on:click=move |_| kit::in_terminal(&h(), "certbot renew --dry-run")>{tr("Tester le renouvellement…", "Test renewal…")}</button>
                <button class="no-drag" on:click=move |_| kit::in_terminal(&h(), "certbot renew")>{tr("Renouveler…", "Renew…")}</button>
                <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                    <Icon name="refresh" size=15 />
                </button>
            </header>
            <div class="main-body">
                <p class="muted small hint">
                    {tr("Alerte dans la cloche de notifications quand un certificat expire dans 15 jours ou moins.", "The notification bell warns when a certificate expires within 15 days.")}
                </p>
                {body}
                <div class="fw-row">
                    <input class="grow mono" placeholder=tr("Surveiller un domaine : exemple.fr ou exemple.fr:8443", "Watch a domain: example.com or example.com:8443") bind:value=new_domain on:keydown=move |e| {
                        if e.key() == "Enter" {
                            add()
                        }
                    } />
                    <button on:click=move |_| add() disabled=move || new_domain.get().trim().is_empty()>{tr("Ajouter", "Add")}</button>
                </div>
            </div>
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_report() {
        let out = "@@needsroot\n@@cert local|example.com|1767225600|C = US, O = Let's Encrypt, CN = R11\n@@cert remote|down.example.org||\n@@cert local|a.example.com|1760000000|C = US, O = Let's Encrypt, CN = E5\n";
        let r = parse(out);
        assert!(r.needs_root);
        assert_eq!(r.certs.len(), 3);
        assert_eq!(r.certs[0].name, "a.example.com");
        assert_eq!(r.certs[1].issuer, "Let's Encrypt R11");
        assert_eq!(r.certs[2].expires, None);
        assert_eq!(r.certs[1].days_left(1767225600 - 86_400 * 10), Some(10));
    }

    #[test]
    fn domains() {
        assert!(valid_domain("example.com"));
        assert!(valid_domain("a.b-c.example.org:8443"));
        assert!(!valid_domain("localhost"));
        assert!(!valid_domain("ex ample.com"));
        assert!(!valid_domain("example.com;rm"));
        assert!(!valid_domain("example.com:99999"));
        assert!(script(&["x.com".into(), "bad;rm".into()]).contains("for d in 'x.com'; do"));
    }
}
