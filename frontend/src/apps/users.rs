//! Users and keys: accounts (with sudo/wheel membership and login shell), the SSH user's
//! authorized_keys (add / remove with the exact command shown) and the last logins.

use leptos::prelude::*;

use crate::apps::files::shell_quote;
use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;

const SCRIPT: &str = r#"echo @@passwd; getent passwd; echo @@group; getent group
echo @@keys; f="$HOME/.ssh/authorized_keys"
if [ -f "$f" ]; then grep -vE '^[[:space:]]*(#|$)' "$f" | while IFS= read -r l; do fp=$(printf '%s\n' "$l" | ssh-keygen -lf /dev/stdin 2>/dev/null | awk '{print $2}'); echo "@@k ${fp:-?}|$l"; done; fi
echo @@last; last -n 40 -w 2>/dev/null"#;

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub home: String,
    pub shell: String,
    pub groups: Vec<String>,
}

impl Account {
    pub fn is_human(&self) -> bool {
        self.uid == 0 || (1000..65534).contains(&self.uid)
    }
    pub fn can_login(&self) -> bool {
        !(self.shell.ends_with("nologin") || self.shell.ends_with("/false") || self.shell.is_empty())
    }
    pub fn is_admin(&self) -> bool {
        self.uid == 0 || self.groups.iter().any(|g| matches!(g.as_str(), "sudo" | "wheel" | "admin"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub line: String,
    pub kind: String,
    pub comment: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Login {
    pub user: String,
    pub tty: String,
    pub from: String,
    pub when: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Data {
    pub accounts: Vec<Account>,
    pub keys: Vec<Key>,
    pub logins: Vec<Login>,
    /// Name of the admin group present on the system ("sudo" or "wheel").
    pub admin_group: String,
}

const KEY_TYPES: [&str; 7] = [
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

/// Splits an authorized_keys line into (type, comment), skipping leading options.
fn key_parts(line: &str) -> (String, String) {
    let toks: Vec<&str> = line.split_whitespace().collect();
    match toks.iter().position(|t| KEY_TYPES.contains(t)) {
        Some(i) => (toks[i].to_string(), toks.get(i + 2..).map(|c| c.join(" ")).unwrap_or_default()),
        None => ("?".to_string(), String::new()),
    }
}

/// A public key pasted by the user: one line, known type, base64 body. Options are refused.
pub fn valid_pubkey(s: &str) -> bool {
    let s = s.trim();
    if s.contains('\n') || s.contains('\r') {
        return false;
    }
    let mut t = s.split_whitespace();
    let (Some(kind), Some(body)) = (t.next(), t.next()) else { return false };
    KEY_TYPES.contains(&kind) && body.len() >= 60 && body.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
}

pub fn valid_username(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase() || f == '_')
        && s.len() <= 32
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_' || x == '-')
}

pub fn parse(out: &str) -> Data {
    let mut d = Data::default();
    let mut section = "";
    let mut groups: Vec<(String, Vec<String>, u32)> = Vec::new();
    for line in out.lines() {
        match line {
            "@@passwd" | "@@group" | "@@keys" | "@@last" => {
                section = match line {
                    "@@passwd" => "passwd",
                    "@@group" => "group",
                    "@@keys" => "keys",
                    _ => "last",
                };
                continue;
            }
            _ => {}
        }
        match section {
            "passwd" => {
                let f: Vec<&str> = line.split(':').collect();
                if f.len() >= 7 {
                    d.accounts.push(Account {
                        name: f[0].into(),
                        uid: f[2].parse().unwrap_or(u32::MAX),
                        home: f[5].into(),
                        shell: f[6].into(),
                        groups: vec![],
                    });
                }
            }
            "group" => {
                let f: Vec<&str> = line.split(':').collect();
                if f.len() >= 4 {
                    let members = f[3].split(',').filter(|m| !m.is_empty()).map(String::from).collect();
                    groups.push((f[0].into(), members, f[2].parse().unwrap_or(u32::MAX)));
                }
            }
            "keys" => {
                if let Some(rest) = line.strip_prefix("@@k ") {
                    if let Some((fp, l)) = rest.split_once('|') {
                        let (kind, comment) = key_parts(l);
                        d.keys.push(Key { line: l.to_string(), kind, comment, fingerprint: fp.to_string() });
                    }
                }
            }
            "last" => {
                let l = line.trim();
                if l.is_empty() || l.starts_with("wtmp") || l.starts_with("reboot") || l.starts_with("shutdown") {
                    continue;
                }
                let f = kit::fields(l, 4);
                if f.len() == 4 {
                    // Local logins have no host column: "user tty Mon Jan 1 …".
                    let local = matches!(f[2].as_str(), "Mon" | "Tue" | "Wed" | "Thu" | "Fri" | "Sat" | "Sun");
                    let (from, when) = if local { (String::new(), format!("{} {}", f[2], f[3])) } else { (f[2].clone(), f[3].clone()) };
                    d.logins.push(Login { user: f[0].clone(), tty: f[1].clone(), from, when });
                }
            }
            _ => {}
        }
    }
    d.admin_group = ["sudo", "wheel"].iter().find(|g| groups.iter().any(|(n, _, _)| n == *g)).map(|g| g.to_string()).unwrap_or_default();
    for a in d.accounts.iter_mut() {
        a.groups = groups.iter().filter(|(_, m, _)| m.contains(&a.name)).map(|(n, _, _)| n.clone()).collect();
    }
    d.accounts.sort_by_key(|a| (!a.is_human(), a.uid));
    d
}

fn add_key_command(key: &str) -> String {
    format!(
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && printf '%s\\n' {} >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys",
        shell_quote(key.trim())
    )
}

fn remove_key_command(line: &str) -> String {
    format!(
        "cp ~/.ssh/authorized_keys ~/.ssh/authorized_keys.bak && {{ grep -vxF -- {} ~/.ssh/authorized_keys.bak > ~/.ssh/authorized_keys || true; }}",
        shell_quote(line)
    )
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Accounts,
    Keys,
    Logins,
}

#[component]
pub fn Users(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let data = RwSignal::new(None::<Data>);
    let sel = RwSignal::new(View::Accounts);
    let show_system = RwSignal::new(false);
    let new_key = RwSignal::new(String::new());
    let new_user = RwSignal::new(String::new());
    let loading = RwSignal::new(false);
    let h = move || host.get_value();
    let me = ctx().info(&host.get_value()).map(|i| i.user).unwrap_or_default();

    let load = move || {
        loading.set(true);
        kit::query(host.get_value(), SCRIPT, move |r| {
            let _ = loading.try_set(false);
            let _ = data.try_set(Some(parse(&r.stdout)));
        });
    };
    load();

    let add_key = move || {
        let k = new_key.get_untracked();
        if !valid_pubkey(&k) {
            ctx().error(tr("Clé publique invalide (ssh-ed25519 AAAA… commentaire).", "Invalid public key (ssh-ed25519 AAAA… comment)."));
            return;
        }
        new_key.set(String::new());
        kit::confirm_run(h(), tr("Ajouter la clé SSH", "Add SSH key"), add_key_command(&k), load);
    };
    let admin_group = move || data.with(|d| d.as_ref().map(|d| d.admin_group.clone()).unwrap_or_default());
    let create_user = move || {
        let u = new_user.get_untracked().trim().to_string();
        if !valid_username(&u) {
            ctx().error(tr("Nom invalide : minuscules, chiffres, - et _.", "Invalid name: lowercase, digits, - and _."));
            return;
        }
        new_user.set(String::new());
        // adduser asks for the password interactively: always in a terminal.
        kit::in_terminal(&h(), &format!("adduser {u}"));
    };

    let accounts = move || {
        let d = data.get().unwrap_or_default();
        let all = show_system.get();
        let grp = d.admin_group.clone();
        let rows: Vec<Account> = d.accounts.into_iter().filter(|a| all || a.is_human()).collect();
        view! {
            <div class="fw-row">
                <label class="check"><input type="checkbox" bind:checked=show_system />{tr("Afficher les comptes système", "Show system accounts")}</label>
                <span class="grow"></span>
                <input class="mono" placeholder=tr("nouvel utilisateur", "new user") bind:value=new_user />
                <button on:click=move |_| create_user() disabled=move || new_user.get().trim().is_empty()>{tr("Créer…", "Create…")}</button>
            </div>
            <div class="table-card">
                <table class="table data-table">
                    <thead><tr><th>{tr("Utilisateur", "User")}</th><th>"UID"</th><th>{tr("Dossier", "Home")}</th><th>{tr("Shell", "Shell")}</th><th>{tr("Groupes", "Groups")}</th><th></th></tr></thead>
                    <tbody>
                        {rows
                            .into_iter()
                            .map(|a| {
                                let admin = a.is_admin();
                                let login = a.can_login();
                                let name = a.name.clone();
                                let grp = grp.clone();
                                let toggle = (!grp.is_empty() && a.uid != 0).then(|| {
                                    let (cmd, label) = if admin {
                                        (format!("gpasswd -d {name} {grp}"), tr("Retirer les droits sudo…", "Remove sudo rights…"))
                                    } else {
                                        (format!("usermod -aG {grp} {name}"), tr("Donner les droits sudo…", "Grant sudo rights…"))
                                    };
                                    view! {
                                        <button class="icon-btn" title=label on:click=move |_| kit::privileged(h(), cmd.clone(), cmd.clone(), load)>
                                            <Icon name="shield" size=14 />
                                        </button>
                                    }
                                });
                                view! {
                                    <tr>
                                        <td class="name-cell">
                                            <div class="cell-flex">
                                                <Icon name="users" size=14 />
                                                <span class="ellipsis">{a.name.clone()}</span>
                                                {admin.then(|| view! { <span class="pill warn">"sudo"</span> })}
                                                {(!login).then(|| view! { <span class="pill off">{tr("sans connexion", "no login")}</span> })}
                                            </div>
                                        </td>
                                        <td class="mono muted">{a.uid}</td>
                                        <td class="mono muted ellipsis">{a.home.clone()}</td>
                                        <td class="mono muted ellipsis">{a.shell.clone()}</td>
                                        <td class="muted small ellipsis">{a.groups.join(", ")}</td>
                                        <td class="row-actions">{toggle}</td>
                                    </tr>
                                }
                            })
                            .collect_view()}
                    </tbody>
                </table>
            </div>
        }
    };

    let me2 = me.clone();
    let keys = move || {
        let d = data.get().unwrap_or_default();
        view! {
            <p class="muted small hint">
                {format!("~/.ssh/authorized_keys {} {}. ", tr("de", "of"), me2)}
                {tr("Ne retirez pas la clé qu'utilise sshOs pour se connecter.", "Don't remove the key sshOs uses to connect.")}
            </p>
            {if d.keys.is_empty() {
                kit::empty_view(tr("Aucune clé autorisée.", "No authorized keys.")).into_any()
            } else {
                view! {
                    <div class="table-card">
                        <table class="table data-table">
                            <thead><tr><th>{tr("Commentaire", "Comment")}</th><th>{tr("Type", "Type")}</th><th>{tr("Empreinte", "Fingerprint")}</th><th></th></tr></thead>
                            <tbody>
                                {d.keys
                                    .into_iter()
                                    .map(|k| {
                                        let line = k.line.clone();
                                        view! {
                                            <tr>
                                                <td class="name-cell"><div class="cell-flex"><Icon name="key" size=14 /><span class="ellipsis">{if k.comment.is_empty() { "—".to_string() } else { k.comment.clone() }}</span></div></td>
                                                <td class="mono muted">{k.kind.clone()}</td>
                                                <td class="mono small ellipsis">{k.fingerprint.clone()}</td>
                                                <td class="row-actions">
                                                    <button class="icon-btn danger" title=tr("Retirer la clé…", "Remove key…") on:click=move |_| {
                                                        kit::confirm_run(h(), tr("Retirer la clé SSH", "Remove SSH key"), remove_key_command(&line), load)
                                                    }>
                                                        <Icon name="trash" size=14 />
                                                    </button>
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
            }}
            <label class="key-add">
                <span class="muted small">{tr("Ajouter une clé publique", "Add a public key")}</span>
                <textarea class="mono" rows="3" spellcheck="false" placeholder="ssh-ed25519 AAAAC3Nza… user@machine" bind:value=new_key></textarea>
            </label>
            <div class="form-actions">
                <button class="primary" on:click=move |_| add_key() disabled=move || !valid_pubkey(&new_key.get())>{tr("Ajouter la clé…", "Add key…")}</button>
            </div>
        }
    };

    let logins = move || {
        let d = data.get().unwrap_or_default();
        if d.logins.is_empty() {
            return kit::empty_view(tr("Aucune connexion enregistrée (wtmp).", "No recorded logins (wtmp).")).into_any();
        }
        view! {
            <div class="table-card">
                <table class="table data-table">
                    <thead><tr><th>{tr("Utilisateur", "User")}</th><th>{tr("Terminal", "Terminal")}</th><th>{tr("Depuis", "From")}</th><th>{tr("Quand", "When")}</th></tr></thead>
                    <tbody>
                        {d.logins
                            .into_iter()
                            .map(|l| {
                                let live = l.when.contains("still logged in");
                                view! {
                                    <tr>
                                        <td>{l.user}</td>
                                        <td class="mono muted">{l.tty}</td>
                                        <td class="mono">{if l.from.is_empty() { tr("local", "local").to_string() } else { l.from }}</td>
                                        <td class="small">{l.when}{live.then(|| view! { " " <span class="pill ok">{tr("connecté", "online")}</span> })}</td>
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

    let title = move || match sel.get() {
        View::Accounts => tr("Comptes", "Accounts"),
        View::Keys => tr("Clés SSH autorisées", "Authorized SSH keys"),
        View::Logins => tr("Dernières connexions", "Last logins"),
    };
    let side_item = move |v: View, label: &'static str, icon: &'static str, n: fn(&Data) -> usize| {
        view! {
            <button class="side-item" class:active=move || sel.get() == v on:click=move |_| sel.set(v)>
                <Icon name=icon size=15 />
                <span class="grow">{label}</span>
                <span class="side-count">{move || data.with(|d| d.as_ref().map(n).unwrap_or(0))}</span>
            </button>
        }
    };

    app_menus(vec![
        MenuSection::new(tr("Utilisateurs", "Users"), vec![MenuItem::new(tr("Actualiser", "Refresh"), load)]),
        MenuSection::new(tr("Présentation", "View"), vec![
            MenuItem::new(tr("Comptes", "Accounts"), move || sel.set(View::Accounts)).checked(move || sel.get() == View::Accounts),
            MenuItem::new(tr("Clés SSH autorisées", "Authorized SSH keys"), move || sel.set(View::Keys)).checked(move || sel.get() == View::Keys),
            MenuItem::new(tr("Dernières connexions", "Last logins"), move || sel.set(View::Logins)).checked(move || sel.get() == View::Logins),
            None,
            MenuItem::new(tr("Comptes système", "System accounts"), move || show_system.update(|s| *s = !*s)).checked(move || show_system.get()),
        ]),
    ]);

    view! {
        <div class="split-app users-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title">{tr("Accès", "Access")}</div>
                {side_item(View::Accounts, tr("Comptes", "Accounts"), "users", |d| d.accounts.iter().filter(|a| a.is_human()).count())}
                {side_item(View::Keys, tr("Clés SSH", "SSH keys"), "key", |d| d.keys.len())}
                {side_item(View::Logins, tr("Connexions", "Logins"), "clock", |d| d.logins.len())}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{title}</h2>
                    {move || (!admin_group().is_empty()).then(|| view! { <span class="muted small">{format!("{} : {}", tr("groupe admin", "admin group"), admin_group())}</span> })}
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    {move || {
                        if data.with(|d| d.is_none()) {
                            return kit::loading_view().into_any();
                        }
                        match sel.get() {
                            View::Accounts => accounts().into_any(),
                            View::Keys => keys().into_any(),
                            View::Logins => logins(),
                        }
                    }}
                </div>
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    #[test]
    fn parse_all() {
        let out = format!(
            "@@passwd\nroot:x:0:0:root:/root:/bin/bash\nwww-data:x:33:33::/var/www:/usr/sbin/nologin\npaul:x:1000:1000::/home/paul:/bin/bash\n@@group\nsudo:x:27:paul\npaul:x:1000:\ndocker:x:999:paul\n@@keys\n@@k SHA256:abc|{K} paul@laptop\n@@k ?|from=\"1.2.3.4\" {K}\n@@last\npaul     pts/0        203.0.113.5      Mon Nov  9 10:00   still logged in\nreboot   system boot  6.1.0            Mon Nov  9 09:00   still running\n\nwtmp begins Sun Nov  1 00:00:00 2026\n"
        );
        let d = parse(&out);
        assert_eq!(d.admin_group, "sudo");
        assert_eq!(d.accounts[0].name, "root");
        assert_eq!(d.accounts[1].name, "paul");
        assert!(d.accounts[1].is_admin());
        assert_eq!(d.accounts[1].groups, vec!["sudo", "docker"]);
        assert!(!d.accounts[2].can_login());
        assert_eq!(d.keys.len(), 2);
        assert_eq!(d.keys[0].comment, "paul@laptop");
        assert_eq!(d.keys[1].kind, "ssh-ed25519");
        assert_eq!(d.logins.len(), 1);
        assert_eq!(d.logins[0].from, "203.0.113.5");
    }

    #[test]
    fn validation() {
        assert!(valid_pubkey(&format!("{K} me@pc")));
        assert!(!valid_pubkey("ssh-ed25519 short"));
        assert!(!valid_pubkey(&format!("command=\"x\" {K}")));
        assert!(!valid_pubkey(&format!("{K}\nssh-rsa AAAA")));
        assert!(valid_username("deploy-1"));
        assert!(!valid_username("Root"));
        assert!(!valid_username("a;rm"));
        assert!(remove_key_command("a'b").contains("'a'\\''b'"));
    }
}
