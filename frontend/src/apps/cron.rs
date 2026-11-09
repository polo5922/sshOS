//! Scheduled tasks: the user's crontab (editable, every line validated and described before it can
//! be saved), the system crontabs (/etc/crontab, /etc/cron.d, read-only) and systemd timers.

use leptos::prelude::*;

use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;

const SCRIPT: &str = r#"echo @@user; crontab -l 2>/dev/null; echo @@file /etc/crontab; cat /etc/crontab 2>/dev/null
for f in /etc/cron.d/*; do [ -f "$f" ] && echo "@@file $f" && cat "$f" 2>/dev/null; done
echo @@timers; systemctl list-timers --all --no-legend --no-pager 2>/dev/null"#;

const EOF: &str = "SSHOS_EOF";

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub schedule: String,
    pub user: String,
    pub command: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timer {
    pub next: String,
    pub last: String,
    pub unit: String,
    pub activates: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Data {
    pub user: String,
    pub system: Vec<Entry>,
    pub timers: Vec<Timer>,
}

const MACROS: [&str; 8] = ["@reboot", "@yearly", "@annually", "@monthly", "@weekly", "@daily", "@midnight", "@hourly"];

/// Splits a crontab line into (schedule, rest). `with_user`: system crontabs have a user column.
fn split_line(line: &str, with_user: bool) -> Option<(String, String, String)> {
    let l = line.trim();
    if l.is_empty() || l.starts_with('#') || is_env(l) {
        return None;
    }
    let n_sched = if l.starts_with('@') { 1 } else { 5 };
    let f = kit::fields(l, n_sched + if with_user { 2 } else { 1 });
    if f.len() < n_sched + if with_user { 2 } else { 1 } {
        return None;
    }
    let schedule = f[..n_sched].join(" ");
    let (user, cmd) = if with_user { (f[n_sched].clone(), f[n_sched + 1].clone()) } else { (String::new(), f[n_sched].clone()) };
    Some((schedule, user, cmd))
}

fn is_env(l: &str) -> bool {
    match l.split_once('=') {
        Some((k, _)) => !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        None => false,
    }
}

pub fn parse(out: &str) -> Data {
    let mut d = Data::default();
    let mut section = String::new();
    let mut user = Vec::new();
    for line in out.lines() {
        if line == "@@user" || line == "@@timers" {
            section = line.to_string();
            continue;
        }
        if let Some(f) = line.strip_prefix("@@file ") {
            section = f.to_string();
            continue;
        }
        match section.as_str() {
            "@@user" => user.push(line),
            "@@timers" => {
                if let Some(t) = parse_timer(line) {
                    d.timers.push(t);
                }
            }
            "" => {}
            file => {
                if let Some((schedule, u, command)) = split_line(line, true) {
                    d.system.push(Entry { schedule, user: u, command, source: file.to_string() });
                }
            }
        }
    }
    d.user = user.join("\n");
    d
}

/// `Mon 2024-01-01 00:00:00 UTC 5h left Sun 2023-12-31 00:00:00 UTC 18h ago logrotate.timer logrotate.service`
/// (`n/a` or `-` replace missing dates).
fn parse_timer(line: &str) -> Option<Timer> {
    let t: Vec<&str> = line.split_whitespace().collect();
    let ui = t.iter().position(|x| x.ends_with(".timer"))?;
    let date_at = |i: usize| -> String {
        match (t.get(i), t.get(i + 1), t.get(i + 2)) {
            (Some(w), Some(d), Some(h)) if w.len() == 3 && w.chars().all(|c| c.is_ascii_alphabetic()) => format!("{d} {h}"),
            _ => "—".to_string(),
        }
    };
    let next = date_at(0);
    let after_left = t.iter().position(|x| *x == "left").map(|i| i + 1).unwrap_or(if t[0] == "n/a" || t[0] == "-" { 3.min(ui) } else { ui });
    let last_start = (after_left..ui).find(|&i| t[i].len() == 3 && t[i].chars().all(|c| c.is_ascii_alphabetic()));
    let last = last_start.map(date_at).unwrap_or_else(|| "—".to_string());
    Some(Timer { next, last, unit: t[ui].to_string(), activates: t.get(ui + 1).map(|s| s.to_string()).unwrap_or_default() })
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn value_ok(v: &str, lo: u32, hi: u32, names: &[&str]) -> bool {
    if let Ok(n) = v.parse::<u32>() {
        return n >= lo && n <= hi;
    }
    names.contains(&v.to_ascii_lowercase().as_str())
}

/// One cron field: `*`, `5`, `1-5`, `*/10`, `1-30/5`, `mon,wed`, comma lists of these.
fn field_ok(f: &str, lo: u32, hi: u32, names: &[&str]) -> bool {
    !f.is_empty()
        && f.split(',').all(|item| {
            let (range, step) = match item.split_once('/') {
                Some((r, s)) => (r, Some(s)),
                None => (item, None),
            };
            if let Some(s) = step {
                if !s.parse::<u32>().is_ok_and(|s| s > 0) {
                    return false;
                }
            }
            if range == "*" {
                return true;
            }
            match range.split_once('-') {
                Some((a, b)) => value_ok(a, lo, hi, names) && value_ok(b, lo, hi, names),
                None => value_ok(range, lo, hi, names),
            }
        })
}

/// Error message for an invalid user-crontab line, or None if the line is fine.
pub fn check_line(line: &str) -> Option<&'static str> {
    let l = line.trim();
    if l.is_empty() || l.starts_with('#') || is_env(l) {
        return None;
    }
    if l.starts_with('@') {
        let mut it = l.splitn(2, char::is_whitespace);
        let m = it.next().unwrap_or("");
        if !MACROS.contains(&m) {
            return Some(tr("Raccourci inconnu (@reboot, @hourly, @daily, @weekly, @monthly, @yearly)", "Unknown shortcut (@reboot, @hourly, @daily, @weekly, @monthly, @yearly)"));
        }
        if it.next().is_none_or(|c| c.trim().is_empty()) {
            return Some(tr("Commande manquante", "Missing command"));
        }
        return None;
    }
    let f = kit::fields(l, 6);
    if f.len() < 6 {
        return Some(tr("Il faut 5 champs (minute heure jour mois jour-semaine) puis la commande", "Needs 5 fields (minute hour day month weekday) then the command"));
    }
    let checks: [(u32, u32, &[&str], &'static str); 5] = [
        (0, 59, &[], tr("Minute invalide (0-59)", "Invalid minute (0-59)")),
        (0, 23, &[], tr("Heure invalide (0-23)", "Invalid hour (0-23)")),
        (1, 31, &[], tr("Jour du mois invalide (1-31)", "Invalid day of month (1-31)")),
        (1, 12, &MONTHS, tr("Mois invalide (1-12 ou jan-dec)", "Invalid month (1-12 or jan-dec)")),
        (0, 7, &DAYS, tr("Jour de la semaine invalide (0-7 ou sun-sat)", "Invalid weekday (0-7 or sun-sat)")),
    ];
    for (i, (lo, hi, names, msg)) in checks.iter().enumerate() {
        if !field_ok(&f[i], *lo, *hi, names) {
            return Some(msg);
        }
    }
    if f[5].contains(EOF) {
        return Some(tr("La commande ne peut pas contenir SSHOS_EOF", "The command cannot contain SSHOS_EOF"));
    }
    None
}

fn day_name(d: &str) -> Option<&'static str> {
    let i = d.parse::<usize>().ok().or_else(|| DAYS.iter().position(|x| x.eq_ignore_ascii_case(d)))?;
    Some(match i % 7 {
        0 => tr("dimanche", "Sunday"),
        1 => tr("lundi", "Monday"),
        2 => tr("mardi", "Tuesday"),
        3 => tr("mercredi", "Wednesday"),
        4 => tr("jeudi", "Thursday"),
        5 => tr("vendredi", "Friday"),
        _ => tr("samedi", "Saturday"),
    })
}

/// Human description of a schedule (5 fields or a macro); `None` for complex expressions.
pub fn describe(schedule: &str) -> Option<String> {
    let s = schedule.trim();
    match s {
        "@reboot" => return Some(tr("Au démarrage du serveur", "At server startup").to_string()),
        "@hourly" => return Some(tr("Toutes les heures", "Every hour").to_string()),
        "@daily" | "@midnight" => return Some(tr("Chaque jour à minuit", "Every day at midnight").to_string()),
        "@weekly" => return Some(tr("Chaque dimanche à minuit", "Every Sunday at midnight").to_string()),
        "@monthly" => return Some(tr("Le 1er de chaque mois à minuit", "On the 1st of every month at midnight").to_string()),
        "@yearly" | "@annually" => return Some(tr("Chaque 1er janvier à minuit", "Every January 1st at midnight").to_string()),
        _ => {}
    }
    let f: Vec<&str> = s.split_whitespace().collect();
    if f.len() != 5 {
        return None;
    }
    let num = |x: &str| x.parse::<u32>().ok();
    let at = |h: u32, m: u32| format!("{h:02}:{m:02}");
    match (f[0], f[1], f[2], f[3], f[4]) {
        ("*", "*", "*", "*", "*") => Some(tr("Toutes les minutes", "Every minute").to_string()),
        (m, "*", "*", "*", "*") if m.starts_with("*/") => Some(format!("{} {} min", tr("Toutes les", "Every"), &m[2..])),
        (m, "*", "*", "*", "*") if num(m).is_some() => Some(format!("{} :{:02}", tr("Chaque heure à", "Every hour at"), num(m)?)),
        (m, h, "*", "*", "*") if h.starts_with("*/") && num(m).is_some() => Some(format!("{} {} h", tr("Toutes les", "Every"), &h[2..])),
        (m, h, "*", "*", "*") if num(m).is_some() && num(h).is_some() => Some(format!("{} {}", tr("Chaque jour à", "Every day at"), at(num(h)?, num(m)?))),
        (m, h, "*", "*", d) if num(m).is_some() && num(h).is_some() && d == "1-5" => {
            Some(format!("{} {}", tr("Du lundi au vendredi à", "Monday to Friday at"), at(num(h)?, num(m)?)))
        }
        (m, h, "*", "*", d) if num(m).is_some() && num(h).is_some() => {
            Some(format!("{} {} {} {}", tr("Chaque", "Every"), day_name(d)?, tr("à", "at"), at(num(h)?, num(m)?)))
        }
        (m, h, dom, "*", "*") if num(m).is_some() && num(h).is_some() && num(dom).is_some() => {
            Some(format!("{} {} {} {}", tr("Le", "On day"), dom, tr("de chaque mois à", "of every month at"), at(num(h)?, num(m)?)))
        }
        _ => None,
    }
}

/// Command that replaces the user's crontab with `text`.
pub fn save_command(text: &str) -> String {
    let body = text.trim_end();
    format!("crontab - <<'{EOF}'\n{body}\n{EOF}")
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Mine,
    System,
    Timers,
}

const PRESETS: [(&str, &str); 6] = [
    ("*/5 * * * *", "5 min"),
    ("0 * * * *", "1 h"),
    ("0 3 * * *", "03:00"),
    ("0 4 * * 0", "dim. / Sun"),
    ("0 2 1 * *", "1/mois · month"),
    ("@reboot", "@reboot"),
];

#[component]
pub fn Cron(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let data = RwSignal::new(None::<Data>);
    let draft = RwSignal::new(String::new());
    let view_sel = RwSignal::new(View::Mine);
    let loading = RwSignal::new(false);
    let new_sched = RwSignal::new("0 3 * * *".to_string());
    let new_cmd = RwSignal::new(String::new());
    let h = move || host.get_value();

    let load = move || {
        loading.set(true);
        kit::query(host.get_value(), SCRIPT, move |r| {
            let d = parse(&r.stdout);
            let _ = draft.try_set(d.user.clone());
            let _ = loading.try_set(false);
            let _ = data.try_set(Some(d));
        });
    };
    load();

    let errors = Memo::new(move |_| {
        draft.with(|t| t.lines().enumerate().filter_map(|(i, l)| check_line(l).map(|e| (i + 1, e))).collect::<Vec<_>>())
    });
    let dirty = move || data.with(|d| d.as_ref().is_some_and(|d| d.user.trim_end() != draft.get().trim_end()));
    let save = move || {
        if !errors.get_untracked().is_empty() {
            return;
        }
        kit::confirm_run(h(), tr("Enregistrer la crontab", "Save crontab"), save_command(&draft.get_untracked()), load);
    };
    let new_line = move || format!("{} {}", new_sched.get().trim(), new_cmd.get().trim());
    let add_line = move || {
        let l = new_line();
        if check_line(&l).is_some() || new_cmd.get_untracked().trim().is_empty() {
            return;
        }
        draft.update(|t| {
            if !t.is_empty() && !t.ends_with('\n') {
                t.push('\n');
            }
            t.push_str(&l);
            t.push('\n');
        });
        new_cmd.set(String::new());
    };

    let mine = move || {
        view! {
            <div class="cron-editor">
                <div class="cron-add">
                    <div class="chips">
                        {PRESETS
                            .iter()
                            .map(|(s, label)| {
                                let s = *s;
                                view! { <button class="chip" class:on=move || new_sched.get() == s on:click=move |_| new_sched.set(s.to_string())>{*label}</button> }
                            })
                            .collect_view()}
                    </div>
                    <div class="fw-row">
                        <input class="mono cron-sched" bind:value=new_sched placeholder="m h dom mon dow" />
                        <input class="mono grow" bind:value=new_cmd placeholder=tr("Commande, ex. /home/me/backup.sh", "Command, e.g. /home/me/backup.sh") />
                        <button on:click=move |_| add_line() disabled=move || new_cmd.get().trim().is_empty() || check_line(&new_line()).is_some()>{tr("Ajouter", "Add")}</button>
                    </div>
                    <div class="muted small">
                        {move || {
                            let l = new_line();
                            match check_line(&l) {
                                Some(e) if !new_cmd.get().trim().is_empty() || !new_sched.get().trim().is_empty() => format!("⚠ {e}"),
                                _ => describe(new_sched.get().trim()).unwrap_or_else(|| tr("Planification personnalisée", "Custom schedule").to_string()),
                            }
                        }}
                    </div>
                </div>
                <textarea class="mono cron-text" spellcheck="false" rows="14" bind:value=draft placeholder=tr("Aucune tâche. Format : minute heure jour mois jour-semaine commande", "No jobs. Format: minute hour day month weekday command")></textarea>
                {move || {
                    let errs = errors.get();
                    (!errs.is_empty())
                        .then(|| {
                            view! {
                                <div class="banner crit">
                                    <Icon name="alert" size=15 />
                                    <div class="grow">
                                        {errs.into_iter().map(|(n, e)| view! { <div>{format!("{} {n} : {e}", tr("Ligne", "Line"))}</div> }).collect_view()}
                                    </div>
                                </div>
                            }
                        })
                }}
                <div class="table-card">
                    <table class="table data-table">
                        <thead><tr><th>{tr("Planification", "Schedule")}</th><th>{tr("Quand", "When")}</th><th>{tr("Commande", "Command")}</th></tr></thead>
                        <tbody>
                            {move || {
                                draft
                                    .get()
                                    .lines()
                                    .filter_map(|l| split_line(l, false))
                                    .map(|(s, _, c)| {
                                        let d = describe(&s).unwrap_or_else(|| "—".into());
                                        view! { <tr><td class="mono">{s}</td><td>{d}</td><td class="mono ellipsis">{c}</td></tr> }
                                    })
                                    .collect_view()
                            }}
                        </tbody>
                    </table>
                </div>
                <div class="form-actions">
                    <button on:click=move |_| draft.set(data.with_untracked(|d| d.as_ref().map(|d| d.user.clone()).unwrap_or_default())) disabled=move || !dirty()>
                        {tr("Annuler les modifications", "Discard changes")}
                    </button>
                    <button class="primary" on:click=move |_| save() disabled=move || !dirty() || !errors.get().is_empty()>{tr("Enregistrer…", "Save…")}</button>
                </div>
            </div>
        }
    };

    let system = move || {
        let rows = data.with(|d| d.as_ref().map(|d| d.system.clone()).unwrap_or_default());
        if rows.is_empty() {
            return kit::empty_view(tr("Aucune tâche système.", "No system jobs.")).into_any();
        }
        view! {
            <div class="table-card">
                <table class="table data-table">
                    <thead><tr><th>{tr("Planification", "Schedule")}</th><th>{tr("Quand", "When")}</th><th>{tr("Utilisateur", "User")}</th><th>{tr("Commande", "Command")}</th><th>{tr("Fichier", "File")}</th></tr></thead>
                    <tbody>
                        {rows
                            .into_iter()
                            .map(|e| {
                                let d = describe(&e.schedule).unwrap_or_else(|| "—".into());
                                let path = e.source.clone();
                                view! {
                                    <tr>
                                        <td class="mono">{e.schedule}</td>
                                        <td>{d}</td>
                                        <td>{e.user}</td>
                                        <td class="mono ellipsis" title=e.command.clone()>{e.command.clone()}</td>
                                        <td>
                                            <button class="link" on:click=move |_| ctx().open(AppKind::TextEdit { path: Some(path.clone()) }, Some(h()))>{e.source}</button>
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

    let timers = move || {
        let rows = data.with(|d| d.as_ref().map(|d| d.timers.clone()).unwrap_or_default());
        if rows.is_empty() {
            return kit::empty_view(tr("Aucun timer systemd.", "No systemd timers.")).into_any();
        }
        view! {
            <div class="table-card">
                <table class="table data-table">
                    <thead><tr><th>{tr("Timer", "Timer")}</th><th>{tr("Lance", "Activates")}</th><th>{tr("Prochaine exécution", "Next run")}</th><th>{tr("Dernière exécution", "Last run")}</th></tr></thead>
                    <tbody>
                        {rows
                            .into_iter()
                            .map(|t| {
                                view! {
                                    <tr>
                                        <td class="name-cell"><div class="cell-flex"><Icon name="clock" size=14 /><span class="ellipsis">{t.unit}</span></div></td>
                                        <td class="muted">{t.activates}</td>
                                        <td class="mono">{t.next}</td>
                                        <td class="mono muted">{t.last}</td>
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

    let title = move || match view_sel.get() {
        View::Mine => tr("Ma crontab", "My crontab"),
        View::System => tr("Tâches système", "System jobs"),
        View::Timers => tr("Timers systemd", "systemd timers"),
    };
    let side_item = move |v: View, label: &'static str, icon: &'static str, n: Box<dyn Fn() -> usize + Send + Sync>| {
        view! {
            <button class="side-item" class:active=move || view_sel.get() == v on:click=move |_| view_sel.set(v)>
                <Icon name=icon size=15 />
                <span class="grow">{label}</span>
                <span class="side-count">{move || n()}</span>
            </button>
        }
    };

    app_menus(vec![
        MenuSection::new(tr("Tâches", "Jobs"), vec![
            MenuItem::new(tr("Actualiser", "Refresh"), load),
            MenuItem::new(tr("Enregistrer la crontab…", "Save crontab…"), save).disabled(move || !dirty()),
        ]),
        MenuSection::new(tr("Présentation", "View"), vec![
            MenuItem::new(tr("Ma crontab", "My crontab"), move || view_sel.set(View::Mine)).checked(move || view_sel.get() == View::Mine),
            MenuItem::new(tr("Tâches système", "System jobs"), move || view_sel.set(View::System)).checked(move || view_sel.get() == View::System),
            MenuItem::new(tr("Timers systemd", "systemd timers"), move || view_sel.set(View::Timers)).checked(move || view_sel.get() == View::Timers),
        ]),
    ]);

    view! {
        <div class="split-app cron-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title">{tr("Planification", "Scheduling")}</div>
                {side_item(View::Mine, tr("Ma crontab", "My crontab"), "clock", Box::new(move || draft.with(|t| t.lines().filter(|l| split_line(l, false).is_some()).count())))}
                {side_item(View::System, tr("Tâches système", "System jobs"), "server", Box::new(move || data.with(|d| d.as_ref().map(|d| d.system.len()).unwrap_or(0))))}
                {side_item(View::Timers, tr("Timers systemd", "systemd timers"), "rotate", Box::new(move || data.with(|d| d.as_ref().map(|d| d.timers.len()).unwrap_or(0))))}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{title}</h2>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    {move || {
                        if data.with(|d| d.is_none()) {
                            return kit::loading_view().into_any();
                        }
                        match view_sel.get() {
                            View::Mine => mine().into_any(),
                            View::System => system(),
                            View::Timers => timers(),
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

    #[test]
    fn validate_lines() {
        assert_eq!(check_line("# comment"), None);
        assert_eq!(check_line("MAILTO=me@example.com"), None);
        assert_eq!(check_line("*/5 * * * * /usr/bin/true"), None);
        assert_eq!(check_line("0 3 * * mon-fri /backup.sh"), None);
        assert_eq!(check_line("0,30 8-18/2 1 jan 1 echo hi"), None);
        assert_eq!(check_line("@reboot /start.sh"), None);
        assert!(check_line("60 * * * * x").is_some());
        assert!(check_line("* 24 * * * x").is_some());
        assert!(check_line("* * * * *").is_some());
        assert!(check_line("@often x").is_some());
        assert!(check_line("*/0 * * * * x").is_some());
    }

    #[test]
    fn describe_schedules() {
        assert_eq!(describe("0 3 * * *").unwrap(), "Chaque jour à 03:00");
        assert_eq!(describe("*/5 * * * *").unwrap(), "Toutes les 5 min");
        assert_eq!(describe("30 6 * * 1").unwrap(), "Chaque lundi à 06:30");
        assert_eq!(describe("0 2 1 * *").unwrap(), "Le 1 de chaque mois à 02:00");
        assert!(describe("1 2 3 4 5").is_none());
    }

    #[test]
    fn parse_all() {
        let out = "@@user\nMAILTO=x\n0 3 * * * /b.sh\n@@file /etc/crontab\nSHELL=/bin/sh\n17 * * * * root cd / && run-parts --report /etc/cron.hourly\n@@file /etc/cron.d/certbot\n0 */12 * * * root certbot -q renew\n@@timers\nMon 2024-01-01 00:00:00 UTC 5h left Sun 2023-12-31 00:00:00 UTC 18h ago logrotate.timer logrotate.service\nn/a n/a n/a n/a snapd.snap-repair.timer snapd.snap-repair.service\n";
        let d = parse(out);
        assert_eq!(d.user, "MAILTO=x\n0 3 * * * /b.sh");
        assert_eq!(d.system.len(), 2);
        assert_eq!(d.system[0].user, "root");
        assert_eq!(d.system[0].command, "cd / && run-parts --report /etc/cron.hourly");
        assert_eq!(d.system[1].source, "/etc/cron.d/certbot");
        assert_eq!(d.timers.len(), 2);
        assert_eq!(d.timers[0].next, "2024-01-01 00:00:00");
        assert_eq!(d.timers[0].last, "2023-12-31 00:00:00");
        assert_eq!(d.timers[1].next, "—");
        assert_eq!(d.timers[1].activates, "snapd.snap-repair.service");
        assert_eq!(save_command("0 3 * * * x\n\n"), "crontab - <<'SSHOS_EOF'\n0 3 * * * x\nSSHOS_EOF");
    }
}
