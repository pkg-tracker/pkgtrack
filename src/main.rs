mod advice;
mod spinner;
mod appstream;
mod health;
mod history;
mod network;
mod sources;
mod startup;
mod usage;
mod views;
mod backends;
mod model;
mod procs;
mod telemetry;
mod update;

use iced::widget::{
    button, center, checkbox, column, container, space, mouse_area, opaque, pick_list, row, scrollable,
    stack, text, text_input, tooltip, Column, Space,
};
use iced::{Alignment, Border, Color, Element, Font, Length, Task, Theme};
use model::{human_size, Details, Kind, Package, Source};
use std::collections::{HashMap, HashSet};
use telemetry::Level;

const BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::DEFAULT
};
const PAGE: usize = 250;

/// Tests that change $HOME must not run at the same time.
#[cfg(test)]
pub(crate) static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().collect();
    if matches!(args.get(1).map(String::as_str), Some("--version" | "-V")) {
        println!("pkgtrack {}", update::CURRENT);
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--check-update") {
        match update::check() {
            Ok(Some(r)) => println!("New version {} available: {}", r.version, r.url),
            Ok(None) => println!("pkgtrack {} is up to date ({})", update::CURRENT, update::repo()),
            Err(e) => eprintln!("update check failed: {e}"),
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--check") {
        cli_check(&args[2..]);
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--notify") {
        // Run by the daily systemd user timer.
        let mut n = 0;
        for s in backends::detect() {
            if let Ok(u) = backends::updates(s) {
                n += u.keys().filter(|k| !k.contains(':')).count();
            }
        }
        println!("{n} updates available");
        if n > 0 {
            let _ = std::process::Command::new("notify-send")
                .args(["--app-name=Package Tracker", "--icon=io.github.pkgtrack", "Updates available", &format!("{n} updates are ready to install. Open Package Tracker to update.")])
                .status();
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--debug") {
        let sources = backends::detect();
        let mut all = Vec::new();
        for &s in &sources {
            all.extend(backends::list(s).unwrap_or_default());
        }
        for p in &mut all {
            p.telemetry = telemetry::quick_level(p);
        }
        match args.get(2).map(String::as_str) {
            Some("net") => {
                network::snapshot();
                std::thread::sleep(std::time::Duration::from_secs(2));
                let s = network::snapshot();
                for p in &s.procs {
                    println!("{} [{}] {}", p.name, p.package.clone().unwrap_or_default(), p.conns.len());
                    for c in p.conns.iter().take(4) {
                        println!("    {:?} {}:{} {} {:?}", c.host, c.remote, c.remote_port, c.state, c.flag);
                    }
                }
                for l in &s.listeners {
                    println!("listen {} {} {} {} public={}", l.proto, l.addr, l.port, l.process, l.public);
                }
            }
            Some("startup") => {
                for i in startup::list() {
                    println!("{:<45} {:<15} on={} active={} {:?} {}", i.id, i.kind.label(), i.enabled, i.active, i.hint, i.why.chars().take(50).collect::<String>());
                }
            }
            Some("sources") => {
                let r = sources::report(&sources, &all);
                for x in &r.repos {
                    println!("{} [{}] enabled={} official={} pkgs={} {:?}", x.name, x.source.short(), x.enabled, x.official, x.installed.len(), x.issues);
                }
                println!("orphans: {}", r.orphans.len());
            }
            Some("history") => {
                let h = history::load(&sources);
                for e in h.iter().take(8) {
                    println!("{} [{}] {} | {} changes | actions: {:?}", e.time, e.source.short(), e.command, e.changes.len(), history::entry_actions(e, &all).iter().map(|a| a.label.clone()).collect::<Vec<_>>());
                }
                println!("total {}", h.len());
            }
            Some("export") => {
                let t = history::export(&all, &sources).unwrap();
                let path = std::env::temp_dir().join("pkgtrack-export-test.txt");
                std::fs::write(&path, &t).unwrap();
                println!("{} lines", t.lines().count());
                let plan = history::plan_import(path.to_str().unwrap(), &all, &sources).unwrap();
                println!("total={} already={} missing={:?} unavailable={}", plan.total, plan.already, plan.missing, plan.unavailable.len());
            }
            Some("update-all") => {
                for s in &sources {
                    if let Ok(map) = backends::updates(*s) {
                        for p in all.iter_mut().filter(|p| p.source == *s) {
                            p.new_version = map.get(&p.id).cloned();
                        }
                    }
                }
                println!("{:?}", advice::update_all_action(&all).map(|a| (a.confirm, advice::describe(&a.kind))));
            }
            Some("appimage") => {
                let path = args.get(3).expect("path");
                let info = backends::appimage::inspect(path).unwrap();
                println!("inspect: name={:?} version={:?} comment={:?} icon={:?} fuse2_missing={} warn={:?}", info.name, info.version, info.comment, info.icon, info.needs_fuse2, info.warning);
                println!("{}", backends::appimage::install(&info).unwrap());
                let list = backends::appimage::list().unwrap();
                for p in &list {
                    println!("listed: {} | {} | {} | {:?} | desktop={}", p.name, p.version, p.id, p.size_bytes.map(model::human_size), p.extra);
                }
                let home = std::env::var("HOME").unwrap();
                for f in std::fs::read_dir(format!("{home}/.local/share/applications")).unwrap().flatten() {
                    println!("--- {}\n{}", f.path().display(), std::fs::read_to_string(f.path()).unwrap().lines().filter(|l| l.starts_with("Exec") || l.starts_with("Icon") || l.starts_with("Name=") || l.starts_with("TryExec") || l.starts_with("X-AppImage")).collect::<Vec<_>>().join("\n"));
                }
                let mine = list.iter().find(|p| p.id.starts_with(&format!("{home}/Applications"))).unwrap();
                println!("{}", backends::appimage::remove(mine).unwrap());
                println!("after remove: {} listed", backends::appimage::list().unwrap().len());
            }
            Some(id) => {
                if let Some(p) = all.iter().find(|p| p.id == id) {
                    let d = backends::details(p);
                    println!("homepage: {:?}\ndescription: {}", d.homepage, d.description.chars().take(400).collect::<String>().replace('\n', " ⏎ "));
                    println!("info: {:?}", d.info.iter().take(3).collect::<Vec<_>>());
                    for s in &d.switches {
                        println!("switch {} on={} {} / {}", s.label, s.on, s.enable, s.disable);
                    }
                }
            }
            None => {}
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--suggest") {
        let sources = backends::detect();
        let mut all = Vec::new();
        for &s in &sources {
            all.extend(backends::list(s).unwrap_or_default());
        }
        let t = std::time::Instant::now();
        let r = advice::scan(all, sources);
        println!("scan took {:?}", t.elapsed());
        println!("\n== PROBLEMS");
        for i in &r.issues {
            println!("[{}] {}\n   {}", i.severity.label(), i.title, i.detail.replace('\n', "\n   "));
            for it in &i.items {
                println!("   - {it}");
            }
            for a in &i.actions {
                println!("   fix: {} -> {}", a.label, advice::describe(&a.kind));
            }
        }
        println!("\n== TELEMETRY");
        for g in &r.telemetry {
            println!("[{}] {} off={:?} fix={:?}\n   -> {}", g.finding.level.label(), g.finding.title, g.finding.off, g.fix.as_ref().map(|a| advice::describe(&a.kind)), g.advice);
            for m in &g.members {
                println!("   {} [{}] {} {:?}", m.pkg.name, m.pkg.source.short(), m.verdict.risk.label(), m.verdict.reasons.first());
            }
        }
        println!("\n== DUPLICATES");
        for g in &r.duplicates {
            println!("{}: {}", g.name, g.advice);
            for m in &g.members {
                println!("   {} [{}] {} keep={} running={} {} {}", m.pkg.id, m.pkg.source.short(), m.pkg.version, m.keep, m.running, m.verdict.risk.label(), m.note);
            }
        }
        println!("\n== UNUSED");
        for u in &r.unused {
            println!("{} [{}] {} {:?} {}", u.member.pkg.name, u.member.pkg.source.short(), u.when, u.size.map(model::human_size), u.member.verdict.risk.label());
        }
        println!("\n== CLEANUP");
        for c in &r.cleanup {
            println!("{} ({} items, {:?}) -> {:?}", c.title, c.items.len(), c.size.map(model::human_size), c.action.as_ref().map(|a| a.label.clone()));
            println!("   {:?}", c.items.iter().take(5).collect::<Vec<_>>());
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--search") {
        let q = args.get(2).cloned().unwrap_or_default();
        for s in backends::detect() {
            match backends::search::search(s, &q) {
                Ok(r) => {
                    println!("{:<8} {} results: {:?}", s.short(), r.len(), r.iter().take(4).map(|p| &p.id).collect::<Vec<_>>());
                    if let Some(first) = r.first() {
                        let (p, d) = backends::search::remote_details(first);
                        println!("         top: {} {} size={:?} also_installs={:?} privacy={:?}", p.id, p.version, p.size_bytes, d.would_install.map(|w| w.len()), d.telemetry.iter().map(|f| &f.title).collect::<Vec<_>>());
                    }
                }
                Err(e) => println!("{:<8} ERROR {e}", s.short()),
            }
        }
        return Ok(());
    }
    let icon =
        iced::window::icon::from_rgba(include_bytes!("../assets/icon-64.rgba").to_vec(), 64, 64).ok();
    iced::application(App::new, App::update, App::view)
        .title("Package Tracker")
        .theme(App::theme)
        .subscription(App::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1600.0, 1000.0),
            min_size: Some(iced::Size::new(1000.0, 600.0)),
            icon,
            // Must match the .desktop file name so Wayland shells show our icon.
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "io.github.pkgtrack".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

/// Headless diagnostics: `pkgtrack --check [package-id...]`
fn cli_check(ids: &[String]) {
    let mut all = Vec::new();
    for s in backends::detect() {
        let t = std::time::Instant::now();
        match backends::list(s) {
            Ok(l) => {
                let apps = l.iter().filter(|p| p.kind == Kind::App).count();
                println!("{:<8} {:>5} packages ({apps} apps) in {:?}", s.short(), l.len(), t.elapsed());
                all.extend(l);
            }
            Err(e) => println!("{:<8} ERROR {e}", s.short()),
        }
        match backends::updates(s) {
            Ok(u) => println!("{:<8} {:>5} updates", "", u.len()),
            Err(e) => println!("{:<8} update check failed: {e}", ""),
        }
    }
    for id in ids {
        for p in all.iter_mut().filter(|p| &p.id == id || &p.name == id) {
            p.telemetry = telemetry::quick_level(p);
            let d = backends::details(p);
            println!("\n== {} [{}] {} {:?} protected={}", p.name, p.source.short(), p.version, p.kind, p.protected);
            println!("used_by({}): {:?}", d.used_by.len(), d.used_by.iter().take(8).collect::<Vec<_>>());
            println!("depends_on: {:?}", d.depends_on.iter().take(8).collect::<Vec<_>>());
            println!("running: {:?}", d.running);
            println!("location: {:?}", d.location);
            println!("would_remove: {:?}", d.would_remove);
            for f in &d.telemetry {
                println!("telemetry [{}] {} — {}", f.level.label(), f.title, f.detail.replace('\n', " | "));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Installed,
    Install,
    Suggestions,
    Network,
    Startup,
    Sources,
    History,
}

/// Which part of the Suggestions page is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    All,
    Problems,
    Unused,
    Telemetry,
    Duplicates,
    Cleanup,
}

const SUGG_SCROLL: &str = "suggestions-scroll";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Apps,
    Updates,
    Privacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Name,
    SizeDesc,
    Newest,
}
impl Sort {
    const ALL: [Sort; 3] = [Sort::Name, Sort::SizeDesc, Sort::Newest];
}
impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Sort::Name => "Sort: Name",
            Sort::SizeDesc => "Sort: Largest first",
            Sort::Newest => "Sort: Recently installed",
        })
    }
}

type Key = (Source, String);

#[derive(Debug, Clone)]
enum Message {
    Loaded(Source, Result<Vec<Package>, String>),
    UpdatesLoaded(Source, Result<HashMap<String, String>, String>),
    Search(String),
    SetSource(Option<Source>),
    SetFilter(Filter),
    ToggleLibs(bool),
    SetSort(Sort),
    ToggleTheme,
    Select(Key),
    DetailsLoaded(Key, Details),
    AskRemove,
    CancelRemove,
    ConfirmRemove,
    Update,
    OpDone(Source, Result<String, String>),
    Refresh,
    ShowMore,
    DismissStatus,
    SetTab(Tab),
    Query(String),
    RunSearch,
    SetSearchSource(Option<Source>),
    SearchDone(u64, Source, Result<Vec<Package>, String>),
    SelectResult(Key),
    RemoteLoaded(Key, Box<(Package, Details)>),
    Install,
    RunAction(advice::Action),
    ConfirmAction,
    CancelAction,
    ActionDone(Result<String, String>),
    ScanSuggestions,
    SuggestionsLoaded(Box<advice::Suggestions>),
    OpenPackage(Key),
    ShowPopup(Key),
    SetSection(Section),
    /// Animation frame (only subscribed while something is loading).
    Tick,
    OpenUrl(String),
    ShowAbout,
    ToggleOutput,
    ClearOutput,
    CopyOutput,
    CloseAbout,
    AppImagePath(String),
    BrowseAppImage,
    InspectAppImage,
    AppImageInspected(Result<Option<Box<backends::appimage::Info>>, String>),
    ClearAppImage,
    PopupLoaded(Key, Box<Details>),
    ClosePopup,
    NetTick,
    NetLoaded(Box<network::Snapshot>),
    SetNetAuto(bool),
    SetNetFlagged(bool),
    StartupLoaded(Vec<startup::Item>),
    SetStartupHints(bool),
    SourcesLoaded(Box<sources::Report>),
    HistoryLoaded(Vec<history::Entry>),
    HistoryQuery(String),
    ToggleEntry(usize),
    HistoryMore,
    Export,
    ImportPick,
    ImportPlanned(Result<Box<history::Plan>, String>),
    ClearImport,
    /// Finished something that didn't change installed software.
    ActionDoneQuiet(Result<String, String>),
    ToggleNotify(bool),
    /// Result of the startup check for a newer Package Tracker release.
    AppUpdateChecked(Result<Option<update::Release>, String>),
    CloseAppUpdate,
    SkipAppUpdate,
    ToggleAppUpdateCheck(bool),
    /// "I understand" on the first-launch disclaimer.
    AcceptDisclaimer,
    /// Tick or untick a package for bulk update.
    ToggleChecked(Key, bool),
    /// Tick every shown package that has an update.
    CheckAllShown,
    ClearChecked,
}

struct App {
    tab: Tab,
    section: Section,
    // --- AppImage install
    appimage_path: String,
    appimage: Option<backends::appimage::Info>,
    appimage_busy: bool,
    // --- package details popup
    popup: Option<Key>,
    popup_details: Option<Details>,
    // --- network tab
    net: Option<network::Snapshot>,
    net_auto: bool,
    net_flagged: bool,
    net_busy: bool,
    // --- startup tab
    startup_items: Option<Vec<startup::Item>>,
    startup_hints_only: bool,
    // --- sources tab
    sources_report: Option<sources::Report>,
    // --- history tab
    history: Option<Vec<history::Entry>>,
    history_query: String,
    history_open: HashSet<usize>,
    history_limit: usize,
    import_plan: Option<history::Plan>,
    notify_on: bool,
    // --- new version of this app
    app_update: Option<update::Release>,
    app_update_check: bool,
    /// Show the "use at your own risk" notice (until accepted once).
    disclaimer: bool,
    /// The About window is open.
    about: bool,
    /// The Output panel at the bottom is open.
    show_output: bool,
    /// What commands printed, newest last (capped).
    output: Vec<String>,
    // --- suggestions tab
    suggestions: Option<advice::Suggestions>,
    scanning: bool,
    pending: Option<advice::Action>,
    // --- install tab
    query: String,
    search_gen: u64,
    searching: HashSet<Source>,
    results: Vec<Package>,
    search_errors: HashMap<Source, String>,
    search_source: Option<Source>,
    r_selected: Option<Key>,
    r_details: Option<(Package, Details)>,
    // --- installed tab
    sources: Vec<Source>,
    packages: Vec<Package>,
    updates: HashMap<Source, HashMap<String, String>>,
    loading: HashSet<Source>,
    checking: HashSet<Source>,
    errors: HashMap<Source, String>,
    search: String,
    source: Option<Source>,
    filter: Filter,
    show_libs: bool,
    sort: Sort,
    dark: bool,
    selected: Option<Key>,
    /// Packages ticked for "Update selected".
    checked: HashSet<Key>,
    details: Option<Details>,
    confirm_remove: bool,
    busy: Option<String>,
    status: Option<(bool, String)>,
    limit: usize,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("background task panicked")
}

fn load(src: Source) -> Task<Message> {
    Task::perform(
        blocking(move || backends::list(src).map_err(|e| e.to_string())),
        move |r| Message::Loaded(src, r),
    )
}

fn check_updates(src: Source) -> Task<Message> {
    Task::perform(
        blocking(move || backends::updates(src).map_err(|e| e.to_string())),
        move |r| Message::UpdatesLoaded(src, r),
    )
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let sources = backends::detect();
        let app = App {
            tab: Tab::Installed,
            section: Section::All,
            appimage_path: String::new(),
            appimage: None,
            appimage_busy: false,
            popup: None,
            popup_details: None,
            net: None,
            net_auto: true,
            net_flagged: false,
            net_busy: false,
            startup_items: None,
            startup_hints_only: false,
            sources_report: None,
            history: None,
            history_query: String::new(),
            history_open: HashSet::new(),
            history_limit: 100,
            import_plan: None,
            notify_on: startup::notify_enabled(),
            app_update: None,
            app_update_check: update::enabled(),
            disclaimer: !update::disclaimer_accepted(),
            about: false,
            show_output: false,
            output: Vec::new(),
            suggestions: None,
            scanning: false,
            pending: None,
            query: String::new(),
            search_gen: 0,
            searching: HashSet::new(),
            results: Vec::new(),
            search_errors: HashMap::new(),
            search_source: None,
            r_selected: None,
            r_details: None,
            loading: sources.iter().copied().collect(),
            checking: sources.iter().copied().collect(),
            sources: sources.clone(),
            packages: Vec::new(),
            updates: HashMap::new(),
            errors: HashMap::new(),
            search: String::new(),
            source: None,
            filter: Filter::All,
            show_libs: false,
            sort: Sort::Name,
            dark: true,
            selected: None,
            checked: HashSet::new(),
            details: None,
            confirm_remove: false,
            busy: None,
            status: None,
            limit: PAGE,
        };
        let tasks = sources.iter().flat_map(|&s| [load(s), check_updates(s)]);
        let self_update = if app.app_update_check {
            Task::perform(blocking(|| update::check().map_err(|e| e.to_string())), Message::AppUpdateChecked)
        } else {
            Task::none()
        };
        // Start maximized (iced 0.13 has no setting for it, so ask once the window exists).
        let maximize = iced::window::oldest().and_then(|id| iced::window::maximize(id, true));
        (app, Task::batch(tasks.chain([maximize, self_update])))
    }

    fn theme(&self) -> Theme {
        if self.dark {
            Theme::CatppuccinMocha
        } else {
            Theme::CatppuccinLatte
        }
    }

    /// Ticked packages that still have an update available.
    fn checked_pkgs(&self) -> Vec<&Package> {
        self.packages
            .iter()
            .filter(|p| p.new_version.is_some() && self.checked.contains(&(p.source, p.id.clone())))
            .collect()
    }

    fn selected_pkg(&self) -> Option<&Package> {
        let (s, id) = self.selected.as_ref()?;
        self.packages.iter().find(|p| p.source == *s && &p.id == id)
    }

    fn apply_updates(&mut self, src: Source) {
        let Some(map) = self.updates.get(&src) else { return };
        for p in self.packages.iter_mut().filter(|p| p.source == src) {
            p.new_version = map
                .get(&p.id)
                .or_else(|| map.get(&p.name))
                .filter(|v| **v != p.version)
                .cloned();
        }
    }

    /// After a source reloads, keep the selection and its details valid.
    fn refresh_selected(&mut self, src: Source) -> Task<Message> {
        if self.selected.as_ref().is_some_and(|k| k.0 == src) {
            if self.selected_pkg().is_none() {
                self.selected = None;
                self.details = None;
            } else if self.details.is_none() {
                return self.fetch_details();
            }
        }
        Task::none()
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        let mut subs = Vec::new();
        if self.tab == Tab::Network && self.net_auto {
            subs.push(iced::time::every(std::time::Duration::from_secs(3)).map(|_| Message::NetTick));
        }
        if self.animating() {
            subs.push(iced::window::frames().map(|_| Message::Tick));
        }
        if self.about {
            subs.push(iced::keyboard::listen().filter_map(|event| match event {
                iced::keyboard::Event::KeyPressed { key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape), .. } => Some(Message::CloseAbout),
                _ => None,
            }));
        } else if self.popup.is_some() {
            subs.push(iced::keyboard::listen().filter_map(|event| match event {
                iced::keyboard::Event::KeyPressed { key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape), .. } => Some(Message::ClosePopup),
                _ => None,
            }));
        } else if self.app_update.is_some() {
            subs.push(iced::keyboard::listen().filter_map(|event| match event {
                iced::keyboard::Event::KeyPressed { key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape), .. } => Some(Message::CloseAppUpdate),
                _ => None,
            }));
        }
        iced::Subscription::batch(subs)
    }

    fn net_refresh(&mut self) -> Task<Message> {
        if self.net_busy {
            return Task::none();
        }
        self.net_busy = true;
        Task::perform(blocking(network::snapshot), |s| Message::NetLoaded(Box::new(s)))
    }

    /// Is a loading animation on screen right now?
    fn animating(&self) -> bool {
        self.busy.is_some()
            || self.appimage_busy
            || (self.popup.is_some() && self.popup_details.is_none())
            || match self.tab {
                Tab::Installed => {
                    (self.packages.is_empty() && !self.loading.is_empty()) || (self.selected.is_some() && self.details.is_none())
                }
                Tab::Install => !self.searching.is_empty() || (self.r_selected.is_some() && self.r_details.is_none()),
                Tab::Suggestions => self.suggestions.is_none(),
                Tab::Network => self.net.is_none(),
                Tab::Startup => self.startup_items.is_none(),
                Tab::Sources => self.sources_report.is_none(),
                Tab::History => self.history.is_none(),
            }
    }

    /// Load the data the current tab needs, if not loaded yet.
    fn load_tab(&mut self) -> Task<Message> {
        match self.tab {
            Tab::Suggestions if self.suggestions.is_none() && self.loading.is_empty() => self.scan(),
            Tab::Network if self.net.is_none() => self.net_refresh(),
            Tab::Startup if self.startup_items.is_none() => {
                Task::perform(blocking(startup::list), Message::StartupLoaded)
            }
            Tab::Sources if self.sources_report.is_none() && self.loading.is_empty() => {
                let (s, p) = (self.sources.clone(), self.packages.clone());
                Task::perform(blocking(move || sources::report(&s, &p)), |r| Message::SourcesLoaded(Box::new(r)))
            }
            Tab::History if self.history.is_none() => {
                let s = self.sources.clone();
                Task::perform(blocking(move || history::load(&s)), Message::HistoryLoaded)
            }
            _ => Task::none(),
        }
    }

    fn scan(&mut self) -> Task<Message> {
        if self.scanning {
            return Task::none();
        }
        self.scanning = true;
        let pkgs = self.packages.clone();
        let sources = self.sources.clone();
        Task::perform(blocking(move || advice::scan(pkgs, sources)), |s| {
            Message::SuggestionsLoaded(Box::new(s))
        })
    }

    fn execute(&mut self, a: advice::Action) -> Task<Message> {
        self.busy = Some(format!("{}…", a.label));
        backends::output::push(format!("\u{25b6} {}", a.label));
        Task::perform(
            blocking(move || {
                advice::run_action(&a.kind)
                    .map(|log| {
                        let last = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
                        if last.len() > 3 && last.len() < 160 { format!("Done: {last}") } else { "Done.".to_string() }
                    })
                    .map_err(|e| e.to_string())
            }),
            Message::ActionDone,
        )
    }

    fn reload_all(&mut self) -> Task<Message> {
        self.loading.extend(self.sources.iter().copied());
        self.checking.extend(self.sources.iter().copied());
        let mut tasks: Vec<_> = self.sources.iter().flat_map(|&s| [load(s), check_updates(s)]).collect();
        tasks.push(self.fetch_details());
        Task::batch(tasks)
    }

    fn fetch_details(&mut self) -> Task<Message> {
        self.details = None;
        let Some(pkg) = self.selected_pkg().cloned() else { return Task::none() };
        let key = (pkg.source, pkg.id.clone());
        Task::perform(blocking(move || backends::details(&pkg)), move |d| {
            Message::DetailsLoaded(key.clone(), d)
        })
    }

    /// Move new command output into the panel's buffer.
    fn drain_output(&mut self) {
        let new = backends::output::drain();
        if new.is_empty() {
            return;
        }
        self.output.extend(new);
        const MAX: usize = 5000;
        if self.output.len() > MAX {
            self.output.drain(..self.output.len() - MAX);
        }
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        self.drain_output();
        match msg {
            Message::Loaded(src, res) => {
                self.loading.remove(&src);
                match res {
                    Ok(mut list) => {
                        self.errors.remove(&src);
                        for p in &mut list {
                            p.telemetry = telemetry::quick_level(p);
                        }
                        self.packages.retain(|p| p.source != src);
                        self.packages.extend(list);
                        self.packages
                            .sort_by_cached_key(|p| (p.name.to_lowercase(), p.source.short()));
                        self.apply_updates(src);
                    }
                    Err(e) => {
                        self.errors.insert(src, e);
                    }
                }
                if self.loading.is_empty() {
                    return Task::batch([self.load_tab(), self.refresh_selected(src)]);
                }
                return self.refresh_selected(src);
            }
            Message::UpdatesLoaded(src, res) => {
                self.checking.remove(&src);
                match res {
                    Ok(map) => {
                        self.updates.insert(src, map);
                        self.apply_updates(src);
                    }
                    Err(e) => {
                        self.errors.insert(src, format!("Update check failed: {e}"));
                    }
                }
            }
            Message::Search(s) => {
                self.search = s;
                self.limit = PAGE;
            }
            Message::SetSource(s) => {
                self.source = s;
                self.limit = PAGE;
            }
            Message::SetFilter(f) => {
                self.filter = f;
                self.limit = PAGE;
            }
            Message::ToggleLibs(b) => self.show_libs = b,
            Message::SetSort(s) => self.sort = s,
            Message::ToggleTheme => self.dark = !self.dark,
            Message::ShowMore => self.limit += PAGE,
            Message::DismissStatus => self.status = None,
            Message::Select(key) => {
                if self.selected.as_ref() == Some(&key) {
                    return Task::none();
                }
                self.selected = Some(key);
                self.confirm_remove = false;
                return self.fetch_details();
            }
            Message::DetailsLoaded(key, d) => {
                if self.selected.as_ref() == Some(&key) {
                    self.details = Some(d);
                }
            }
            Message::ToggleChecked(key, on) => {
                if on {
                    self.checked.insert(key);
                } else {
                    self.checked.remove(&key);
                }
            }
            Message::CheckAllShown => {
                let keys: Vec<Key> = self
                    .visible()
                    .into_iter()
                    .filter(|p| p.new_version.is_some())
                    .map(|p| (p.source, p.id.clone()))
                    .collect();
                self.checked.extend(keys);
            }
            Message::ClearChecked => self.checked.clear(),
            Message::AskRemove => self.confirm_remove = true,
            Message::CancelRemove => self.confirm_remove = false,
            Message::ConfirmRemove | Message::Update => {
                let removing = matches!(msg, Message::ConfirmRemove);
                self.confirm_remove = false;
                let Some(pkg) = self.selected_pkg().cloned() else { return Task::none() };
                self.busy = Some(format!(
                    "{} {}…",
                    if removing { "Removing" } else { "Updating" },
                    pkg.name
                ));
                let src = pkg.source;
                return Task::perform(
                    blocking(move || {
                        let r = if removing { backends::remove(&pkg) } else { backends::update(&pkg) };
                        r.map(|_| {
                            format!("{} {}", pkg.name, if removing { "was removed." } else { "was updated." })
                        })
                        .map_err(|e| e.to_string())
                    }),
                    move |r| Message::OpDone(src, r),
                );
            }
            Message::OpDone(src, res) => {
                self.busy = None;
                let ok = res.is_ok();
                self.status = Some(match res {
                    Ok(m) => (true, m),
                    Err(e) => (false, e),
                });
                if ok {
                    self.details = None;
                    self.loading.insert(src);
                    self.checking.insert(src);
                    return Task::batch([load(src), check_updates(src)]);
                }
            }
            Message::SetTab(t) => {
                self.tab = t;
                return self.load_tab();
            }
            Message::NetTick => return self.net_refresh(),
            Message::NetLoaded(snap) => {
                self.net_busy = false;
                self.net = Some(*snap);
            }
            Message::SetNetAuto(b) => self.net_auto = b,
            Message::SetNetFlagged(b) => self.net_flagged = b,
            Message::StartupLoaded(items) => self.startup_items = Some(items),
            Message::SetStartupHints(b) => self.startup_hints_only = b,
            Message::SourcesLoaded(r) => self.sources_report = Some(*r),
            Message::HistoryLoaded(h) => self.history = Some(h),
            Message::HistoryQuery(q) => {
                self.history_query = q;
                self.history_limit = 100;
            }
            Message::ToggleEntry(i) => {
                if !self.history_open.remove(&i) {
                    self.history_open.insert(i);
                }
            }
            Message::HistoryMore => self.history_limit += 100,
            Message::AppUpdateChecked(r) => match r {
                Ok(rel) => self.app_update = rel,
                // Offline or GitHub unreachable: not worth bothering the user.
                Err(e) => eprintln!("update check failed: {e}"),
            },
            Message::CloseAppUpdate => self.app_update = None,
            Message::ShowAbout => self.about = true,
            Message::ToggleOutput => self.show_output = !self.show_output,
            Message::ClearOutput => self.output.clear(),
            Message::CopyOutput => return iced::clipboard::write(self.output.join("\n")),
            Message::CloseAbout => self.about = false,
            Message::AcceptDisclaimer => {
                self.disclaimer = false;
                if let Err(e) = update::accept_disclaimer() {
                    self.status = Some((false, format!("Couldn't save that you accepted the notice; it will show again next time: {e}")));
                }
            }
            Message::SkipAppUpdate => {
                if let Some(rel) = self.app_update.take() {
                    if let Err(e) = update::skip(&rel.version) {
                        self.status = Some((false, format!("Could not save setting: {e}")));
                    }
                }
            }
            Message::ToggleAppUpdateCheck(on) => match update::set_enabled(on) {
                Ok(()) => {
                    self.app_update_check = on;
                    if on {
                        return Task::perform(blocking(|| update::check().map_err(|e| e.to_string())), Message::AppUpdateChecked);
                    }
                    self.app_update = None;
                }
                Err(e) => self.status = Some((false, format!("Could not save setting: {e}"))),
            },
            Message::ToggleNotify(on) => {
                self.notify_on = on;
                return self.execute(advice::Action {
                    label: if on { "Turning on daily update check".into() } else { "Turning off daily update check".into() },
                    kind: advice::ActionKind::Notify(on),
                    confirm: None,
                });
            }
            Message::Export => {
                let pkgs = self.packages.clone();
                let sources = self.sources.clone();
                self.busy = Some("Exporting app list…".into());
                return Task::perform(
                    blocking(move || -> Result<String, String> {
                        let date = backends::run("date", &["+%F"], &[]).unwrap_or_default().trim().to_string();
                        let home = std::env::var("HOME").unwrap_or_default();
                        let default = format!("{home}/pkgtrack-apps-{date}.txt");
                        let path = if backends::which("zenity") {
                            let out = std::process::Command::new("zenity")
                                .args(["--file-selection", "--save", "--confirm-overwrite", &format!("--filename={default}")])
                                .output()
                                .map_err(|e| e.to_string())?;
                            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                            if p.is_empty() {
                                return Err("Export cancelled.".into());
                            }
                            p
                        } else {
                            default
                        };
                        let text = history::export(&pkgs, &sources).map_err(|e| e.to_string())?;
                        let n = text.lines().filter(|l| !l.starts_with('#')).count();
                        std::fs::write(&path, text).map_err(|e| e.to_string())?;
                        Ok(format!("Saved {n} apps and packages to {path}"))
                    }),
                    Message::ActionDoneQuiet,
                );
            }
            Message::ImportPick => {
                let pkgs = self.packages.clone();
                let sources = self.sources.clone();
                return Task::perform(
                    blocking(move || -> Result<Box<history::Plan>, String> {
                        let home = std::env::var("HOME").unwrap_or_default();
                        let path = if backends::which("zenity") {
                            let out = std::process::Command::new("zenity")
                                .args(["--file-selection", "--title=Choose an app list", &format!("--filename={home}/")])
                                .output()
                                .map_err(|e| e.to_string())?;
                            String::from_utf8_lossy(&out.stdout).trim().to_string()
                        } else {
                            // Without a file dialog, use the newest export in the home folder.
                            let mut files: Vec<String> = std::fs::read_dir(&home)
                                .map_err(|e| e.to_string())?
                                .flatten()
                                .map(|e| e.file_name().to_string_lossy().into_owned())
                                .filter(|n| n.starts_with("pkgtrack-apps-") && n.ends_with(".txt"))
                                .collect();
                            files.sort();
                            files.pop().map(|f| format!("{home}/{f}")).unwrap_or_default()
                        };
                        if path.is_empty() {
                            return Err("No file chosen.".into());
                        }
                        history::plan_import(&path, &pkgs, &sources).map(Box::new).map_err(|e| e.to_string())
                    }),
                    Message::ImportPlanned,
                );
            }
            Message::ImportPlanned(r) => match r {
                Ok(p) => self.import_plan = Some(*p),
                Err(e) => self.status = Some((false, e)),
            },
            Message::ClearImport => self.import_plan = None,
            Message::ActionDoneQuiet(res) => {
                self.busy = None;
                self.status = Some(match res {
                    Ok(m) => (true, m),
                    Err(e) => (false, e),
                });
            }
            Message::ScanSuggestions => {
                self.suggestions = None;
                return self.scan();
            }
            Message::SuggestionsLoaded(s) => {
                self.scanning = false;
                self.suggestions = Some(*s);
            }
            Message::RunAction(a) => {
                if a.confirm.is_some() {
                    self.pending = Some(a);
                } else {
                    return self.execute(a);
                }
            }
            Message::CancelAction => self.pending = None,
            Message::ConfirmAction => {
                if let Some(a) = self.pending.take() {
                    return self.execute(a);
                }
            }
            Message::ActionDone(res) => {
                self.busy = None;
                let ok = res.is_ok();
                self.output.push(match &res {
                    Ok(m) => format!("\u{2714} {m}"),
                    Err(e) => format!("\u{2716} {}", e.lines().next().unwrap_or("Failed")),
                });
                self.output.push(String::new());
                self.status = Some(match res {
                    Ok(m) => (true, m),
                    Err(e) => (false, e),
                });
                if ok {
                    self.checked.clear();
                    self.appimage = None;
                    self.appimage_path.clear();
                    self.popup = None;
                    self.popup_details = None;
                    self.suggestions = None;
                    self.details = None;
                    self.startup_items = None;
                    self.sources_report = None;
                    self.history = None;
                    self.import_plan = None;
                    self.notify_on = startup::notify_enabled();
                    return self.reload_all();
                }
            }
            Message::Tick => {}
            Message::SetSection(s) => {
                self.section = s;
                return iced::widget::operation::snap_to(SUGG_SCROLL, iced::widget::operation::RelativeOffset::START);
            }
            Message::OpenUrl(url) => {
                if url.starts_with("https://") || url.starts_with("http://") {
                    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
                }
            }
            Message::AppImagePath(p) => self.appimage_path = p,
            Message::BrowseAppImage | Message::InspectAppImage => {
                let browse = matches!(msg, Message::BrowseAppImage);
                let typed = self.appimage_path.trim().to_string();
                self.appimage_busy = true;
                self.tab = Tab::Install;
                return Task::perform(
                    blocking(move || -> Result<Option<Box<backends::appimage::Info>>, String> {
                        let path = if browse {
                            if !backends::which("zenity") {
                                return Err("No file dialog available (install zenity), or paste the file path instead.".into());
                            }
                            let home = std::env::var("HOME").unwrap_or_default();
                            let out = std::process::Command::new("zenity")
                                .args([
                                    "--file-selection",
                                    "--title=Choose an AppImage",
                                    &format!("--filename={home}/Downloads/"),
                                    "--file-filter=AppImage | *.AppImage *.appimage",
                                    "--file-filter=All files | *",
                                ])
                                .output()
                                .map_err(|e| e.to_string())?;
                            String::from_utf8_lossy(&out.stdout).trim().to_string()
                        } else {
                            typed.replace("file://", "").trim_matches(['\'', '"']).to_string()
                        };
                        if path.is_empty() {
                            return Ok(None);
                        }
                        backends::appimage::inspect(&path).map(|i| Some(Box::new(i))).map_err(|e| e.to_string())
                    }),
                    Message::AppImageInspected,
                );
            }
            Message::AppImageInspected(r) => {
                self.appimage_busy = false;
                match r {
                    Ok(Some(info)) => {
                        self.appimage_path = info.original.clone();
                        self.appimage = Some(*info);
                    }
                    Ok(None) => {}
                    Err(e) => self.status = Some((false, e)),
                }
            }
            Message::ClearAppImage => {
                if let Some(i) = self.appimage.take() {
                    if let Some(dir) = std::path::Path::new(&i.staged).parent() {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                }
            }
            Message::ShowPopup(key) => {
                self.popup = Some(key.clone());
                self.popup_details = None;
                self.pending = None;
                let Some(pkg) = self.packages.iter().find(|p| p.source == key.0 && p.id == key.1).cloned() else {
                    return Task::none();
                };
                return Task::perform(blocking(move || backends::details(&pkg)), move |d| {
                    Message::PopupLoaded(key.clone(), Box::new(d))
                });
            }
            Message::PopupLoaded(key, d) => {
                if self.popup.as_ref() == Some(&key) {
                    self.popup_details = Some(*d);
                }
            }
            Message::ClosePopup => {
                self.popup = None;
                self.popup_details = None;
                self.pending = None;
            }
            Message::OpenPackage(key) => {
                self.popup = None;
                self.popup_details = None;
                self.tab = Tab::Installed;
                self.source = None;
                self.filter = Filter::All;
                self.search.clear();
                self.selected = None;
                return self.update(Message::Select(key));
            }
            Message::Query(q) => self.query = q,
            Message::SetSearchSource(s) => self.search_source = s,
            Message::RunSearch => {
                let q = self.query.trim().to_string();
                if q.len() < 2 {
                    return Task::none();
                }
                self.search_gen += 1;
                self.results.clear();
                self.search_errors.clear();
                self.r_selected = None;
                self.r_details = None;
                self.searching = self.sources.iter().copied().collect();
                let generation = self.search_gen;
                return Task::batch(self.sources.iter().map(|&src| {
                    let q = q.clone();
                    Task::perform(
                        blocking(move || backends::search::search(src, &q).map_err(|e| e.to_string())),
                        move |r| Message::SearchDone(generation, src, r),
                    )
                }));
            }
            Message::SearchDone(generation, src, res) => {
                if generation != self.search_gen {
                    return Task::none(); // stale result from an older query
                }
                self.searching.remove(&src);
                match res {
                    Ok(list) => {
                        self.results.extend(list);
                        backends::search::rank(&mut self.results, self.query.trim());
                    }
                    Err(e) => {
                        self.search_errors.insert(src, e);
                    }
                }
            }
            Message::SelectResult(key) => {
                if self.r_selected.as_ref() == Some(&key) {
                    return Task::none();
                }
                self.r_selected = Some(key.clone());
                self.r_details = None;
                let Some(pkg) = self.results.iter().find(|p| p.source == key.0 && p.id == key.1).cloned()
                else {
                    return Task::none();
                };
                return Task::perform(blocking(move || backends::search::remote_details(&pkg)), move |r| {
                    Message::RemoteLoaded(key.clone(), Box::new(r))
                });
            }
            Message::RemoteLoaded(key, r) => {
                if self.r_selected.as_ref() == Some(&key) {
                    self.r_details = Some(*r);
                }
            }
            Message::Install => {
                let Some((pkg, _)) = self.r_details.clone() else { return Task::none() };
                self.busy = Some(format!("Installing {}…", pkg.name));
                let src = pkg.source;
                return Task::perform(
                    blocking(move || {
                        backends::search::install(&pkg)
                            .map(|_| format!("{} was installed.", pkg.name))
                            .map_err(|e| e.to_string())
                    }),
                    move |r| Message::OpDone(src, r),
                );
            }
            Message::Refresh => {
                self.suggestions = None;
                return self.reload_all();
            }
        }
        Task::none()
    }

    fn base_filtered(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter().filter(move |p| {
            self.source.is_none_or(|s| s == p.source) && (self.show_libs || p.kind != Kind::Library)
        })
    }

    fn visible(&self) -> Vec<&Package> {
        let q = self.search.to_lowercase();
        let mut v: Vec<&Package> = self
            .base_filtered()
            .filter(|p| match self.filter {
                Filter::All => true,
                Filter::Apps => p.kind == Kind::App,
                Filter::Updates => p.new_version.is_some(),
                Filter::Privacy => p.telemetry.is_some(),
            })
            .filter(|p| {
                q.is_empty()
                    || p.name.to_lowercase().contains(&q)
                    || p.id.to_lowercase().contains(&q)
                    || p.summary.to_lowercase().contains(&q)
            })
            .collect();
        if self.sort == Sort::Newest {
            v.sort_by(|a, b| b.installed_at.cmp(&a.installed_at));
        }
        if self.sort == Sort::SizeDesc {
            v.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        }
        v
    }

    // ---------------------------------------------------------------- view

    fn view(&self) -> Element<'_, Message> {
        let base = match self.tab {
            Tab::Installed => row![self.sidebar(), self.list_view(), self.detail_view()],
            Tab::Install => row![self.sidebar(), self.search_view(), self.remote_detail_view()],
            Tab::Suggestions => row![self.sidebar(), self.suggestions_view()],
            Tab::Network => row![self.sidebar(), self.network_view()],
            Tab::Startup => row![self.sidebar(), self.startup_view()],
            Tab::Sources => row![self.sidebar(), self.sources_view()],
            Tab::History => row![self.sidebar(), self.history_view()],
        }
        .height(Length::Fill);
        let page: Element<Message> = match (&self.popup, &self.app_update) {
            // First launch: nothing else until the risk notice is confirmed.
            _ if self.disclaimer => modal(base.into(), self.disclaimer_view(), Message::Tick),
            _ if self.about => modal(base.into(), self.about_view(), Message::CloseAbout),
            (Some(key), _) => modal(base.into(), self.popup_view(key), Message::ClosePopup),
            (None, Some(rel)) => modal(base.into(), self.app_update_view(rel), Message::CloseAppUpdate),
            (None, None) => base.into(),
        };
        let main: Element<Message> = match &self.busy {
            // Something is running (uninstall, update, cleanup…): dim the window and show progress.
            Some(what) => stack![
                page,
                opaque(center(
                    container(
                        column![
                            spinner::spinner(190.0),
                            text(what.clone()).size(18).font(BOLD),
                            muted("If a password prompt appears, enter your password to continue.".into()).size(12),
                            button(text(if self.show_output { "Hide output" } else { "Show output" }).size(12))
                                .style(button::text)
                                .on_press(Message::ToggleOutput),
                        ]
                        .spacing(10)
                        .align_x(Alignment::Center),
                    )
                    .padding(28)
                    .style(|t: &Theme| container::Style {
                        background: Some(t.extended_palette().background.base.color.into()),
                        border: Border { radius: 16.0.into(), width: 1.0, color: t.extended_palette().background.strong.color },
                        // No shadow: see popup_frame in views.rs.
                        ..Default::default()
                    }),
                )
                .style(|_| container::Style { background: Some(Color { a: 0.45, ..Color::BLACK }.into()), ..Default::default() }))
            ]
            .into(),
            None => page,
        };
        if self.show_output {
            // Outside the overlay, so it stays readable while something runs.
            column![container(main).height(Length::Fill), self.output_panel()].into()
        } else {
            main
        }
    }

    fn output_panel(&self) -> Element<'_, Message> {
        let header = row![
            text("Output").size(13).font(BOLD),
            muted(if self.busy.is_some() { "running…".into() } else { format!("{} lines", self.output.len()) }).size(11),
            iced::widget::space::horizontal(),
            button(text("Copy").size(12)).style(button::secondary).padding([3, 10])
                .on_press_maybe((!self.output.is_empty()).then_some(Message::CopyOutput)).tip("Copy all output, e.g. for a bug report"),
            button(text("Clear").size(12)).style(button::secondary).padding([3, 10])
                .on_press_maybe((!self.output.is_empty() && self.busy.is_none()).then_some(Message::ClearOutput)).tip("Empty the panel"),
            button(text("Hide").size(12)).style(button::secondary).padding([3, 10]).on_press(Message::ToggleOutput).tip("Close the panel"),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let body: Element<Message> = if self.output.is_empty() {
            muted("Commands that Package Tracker runs for you, and what they print, show up here as they happen.".into()).size(12).into()
        } else {
            let mut col = Column::new().spacing(1);
            for l in &self.output {
                let t = text(l.as_str()).size(12).font(Font::MONOSPACE);
                col = col.push(match l.chars().next() {
                    Some('$') => t.style(|th: &Theme| text::Style { color: Some(th.extended_palette().primary.base.color) }),
                    Some('\u{2714}') => t.style(text::success),
                    Some('\u{2716}') => t.style(text::danger),
                    Some('\u{25b6}') => t.font(Font { weight: iced::font::Weight::Bold, ..Font::MONOSPACE }),
                    _ => t,
                });
            }
            scrollable(container(col).padding(iced::Padding { right: 12.0, ..Default::default() }))
                .anchor_bottom()
                .height(Length::Fill)
                .width(Length::Fill)
                .into()
        };
        container(column![header, body].spacing(6))
            .padding([8, 14])
            .height(Length::Fixed(230.0))
            .width(Length::Fill)
            .style(|t: &Theme| container::Style {
                background: Some(t.extended_palette().background.weak.color.into()),
                border: Border { width: 1.0, color: t.extended_palette().background.strong.color, ..Default::default() },
                ..Default::default()
            })
            .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let count = |s: Option<Source>| self.packages.iter().filter(|p| s.is_none_or(|s| p.source == s)).count();
        let mut col = Column::new()
            .spacing(4)
            .push(
                row![
                    iced::widget::image(iced::widget::image::Handle::from_rgba(
                        64,
                        64,
                        include_bytes!("../assets/icon-64.rgba").as_slice(),
                    ))
                    .width(34)
                    .height(34),
                    text("Package Tracker").size(20).font(BOLD),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .push(muted(format!("{} packages across {} sources", self.packages.len(), self.sources.len())).size(12))
            .push(Space::new().height(14))
            .push({
                let t = |label, tab| {
                    let help = match tab {
                        Tab::Installed => "Everything installed: details, updates and uninstall",
                        Tab::Install => "Search for new software and install it",
                        Tab::Suggestions => "Problems to fix, what to clean up, what's duplicated, and trackers to turn off",
                        Tab::Network => "Which programs are talking to the internet right now",
                        Tab::Startup => "Services and apps that start automatically",
                        Tab::Sources => "Repositories your software comes from",
                        Tab::History => "What was installed, updated and removed; export and import your app list",
                    };
                    tab_button(label, self.tab == tab, Message::SetTab(tab)).tip(help)
                };
                column![
                    row![t("Installed", Tab::Installed), t("Install", Tab::Install)].spacing(4),
                    row![t("Suggestions", Tab::Suggestions), t("Network", Tab::Network)].spacing(4),
                    row![t("Startup", Tab::Startup), t("Sources", Tab::Sources)].spacing(4),
                    row![t("History", Tab::History), Space::new().width(Length::Fill)].spacing(4),
                ]
                .spacing(4)
            })
            .push(Space::new().height(14));
        if let Some(side) = self.extra_sidebar() {
            return self.sidebar_frame(col.push(side).push(Space::new().height(Length::Fill)));
        }
        if self.tab == Tab::Install {
            return self.search_sidebar(col);
        }
        if self.tab == Tab::Suggestions {
            let col = col
                .push(muted("Suggestions look at your whole system: telemetry you can turn off, apps installed twice, and leftovers that are safe to clean up.".into()).size(12))
                .push(Space::new().height(10))
                .push(muted("Every removal shows what it affects and asks before doing anything.".into()).size(12))
                .push(Space::new().height(Length::Fill));
            return self.sidebar_frame(col);
        }
        col = col
            .push(section_title("SOURCES"))
            .push(nav_item("All sources".into(), count(None).to_string(), self.source.is_none(), Message::SetSource(None)).tip("Show packages from every source"));
        for &s in &self.sources {
            let badge = if self.loading.contains(&s) {
                "loading…".to_string()
            } else if self.errors.contains_key(&s) {
                "error".to_string()
            } else {
                count(Some(s)).to_string()
            };
            col = col.push(nav_item(s.label().into(), badge, self.source == Some(s), Message::SetSource(Some(s))).tip(format!("Show only {} packages", s.label())));
        }
        let n_upd = self.base_filtered().filter(|p| p.new_version.is_some()).count();
        let n_priv = self.base_filtered().filter(|p| p.telemetry.is_some()).count();
        let n_apps = self.base_filtered().filter(|p| p.kind == Kind::App).count();
        let checking = !self.checking.is_empty();
        col = col
            .push(Space::new().height(14))
            .push(section_title("SHOW"))
            .push(nav_item("Everything".into(), String::new(), self.filter == Filter::All, Message::SetFilter(Filter::All)).tip("No filter"))
            .push(nav_item("Desktop apps".into(), n_apps.to_string(), self.filter == Filter::Apps, Message::SetFilter(Filter::Apps)).tip("Only apps that have a launcher in your app menu"))
            .push(nav_item(
                "Updates available".into(),
                if checking { format!("{n_upd}…") } else { n_upd.to_string() },
                self.filter == Filter::Updates,
                Message::SetFilter(Filter::Updates),
            ).tip("Only packages with a newer version available"))
            .push(nav_item("Privacy flagged".into(), n_priv.to_string(), self.filter == Filter::Privacy, Message::SetFilter(Filter::Privacy)).tip("Only packages with known telemetry"))
            .push(Space::new().height(14))
            .push(checkbox(self.show_libs).label("Include libraries").on_toggle(Message::ToggleLibs).size(16).text_size(14).tip("Also list shared libraries (hidden by default; there are thousands)"))
            .push(Space::new().height(10))
            .push(checkbox(self.notify_on).label("Daily update notification").on_toggle(Message::ToggleNotify).size(16).text_size(14).tip("Show a desktop notification once a day when updates are available, even when this app is closed"))
            .push(Space::new().height(10))
            .push(checkbox(self.app_update_check).label("Check for app updates").on_toggle(Message::ToggleAppUpdateCheck).size(16).text_size(14).tip("At startup, ask GitHub whether a newer version of Package Tracker is out (nothing about your computer is sent)"));
        if let Some(a) = advice::update_all_action(&self.packages).filter(|_| self.checking.is_empty()) {
            col = col.push(Space::new().height(10)).push(
                {
                    let help = format!("Install every available update. Runs: {}", advice::describe(&a.kind));
                    button(text(format!("Update all ({n_upd})")).size(14))
                        .width(Length::Fill)
                        .padding([8, 12])
                        .style(button::success)
                        .on_press_maybe((self.busy.is_none() && self.pending.is_none()).then_some(Message::RunAction(a)))
                        .tip(help)
                },
            );
        }
        col = col.push(Space::new().height(Length::Fill));
        for (s, e) in &self.errors {
            col = col.push(text(format!("{}: {}", s.short(), e)).size(11).style(text::danger));
        }
        self.sidebar_frame(col)
    }

    fn sidebar_frame<'a>(&'a self, col: Column<'a, Message>) -> Element<'a, Message> {
        let col = col.push(
            button(text(if self.show_output { "Hide output" } else { "Show output" }).size(13))
                .on_press(Message::ToggleOutput)
                .style(button::secondary)
                .width(Length::Fill)
                .tip("A panel at the bottom with the commands Package Tracker runs and what they print"),
        );
        let col = col.push(
            button(text("About").size(13))
                .on_press(Message::ShowAbout)
                .style(button::secondary)
                .width(Length::Fill)
                .tip("Version, who makes Package Tracker, and what it does"),
        );
        let col = col.push(
            button(text(if self.dark { "Light theme" } else { "Dark theme" }).size(13))
                .on_press(Message::ToggleTheme)
                .style(button::secondary)
                .width(Length::Fill).tip("Switch between dark and light colours"),
        );
        container(col)
            .padding(18)
            .width(Length::Fixed(250.0))
            .height(Length::Fill)
            .style(|t: &Theme| container::Style {
                background: Some(t.extended_palette().background.weak.color.scale_alpha(0.35).into()),
                ..Default::default()
            })
            .into()
    }

    fn list_view(&self) -> Element<'_, Message> {
        let visible = self.visible();
        let toolbar = row![
            text_input("Search packages, apps, descriptions…", &self.search)
                .on_input(Message::Search)
                .padding(10)
                .size(15),
            pick_list(&Sort::ALL[..], Some(self.sort), Message::SetSort).padding(10).text_size(14),
            button(text("Refresh").size(14))
                .padding([10, 16])
                .style(button::secondary)
                .on_press_maybe(self.loading.is_empty().then_some(Message::Refresh)).tip("Reload all packages and check for updates again"),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        // Rows with an update get a tick box for "Update selected"; the
        // others get an empty slot of the same width so names line up.
        let any_updates = visible.iter().any(|p| p.new_version.is_some());
        let mut list = Column::new().spacing(2);
        for p in visible.iter().take(self.limit) {
            let item = self.pkg_row(p);
            list = list.push(if !any_updates {
                item
            } else {
                let key: Key = (p.source, p.id.clone());
                let tick: Element<Message> = if p.new_version.is_some() {
                    let on = self.checked.contains(&key);
                    checkbox(on)
                        .on_toggle(move |b| Message::ToggleChecked(key.clone(), b))
                        .size(18)
                        .tip(format!("Select {} for bulk update", p.name))
                } else {
                    Space::new().width(18).into()
                };
                row![container(tick).width(Length::Fixed(26.0)).center_x(Length::Fixed(26.0)), item]
                    .spacing(4)
                    .align_y(Alignment::Center)
                    .into()
            });
        }
        if visible.len() > self.limit {
            list = list.push(
                container(
                    button(text(format!("Show more ({} remaining)", visible.len() - self.limit)).size(13))
                        .on_press(Message::ShowMore)
                        .style(button::secondary).tip("Show the next 250 packages"),
                )
                .padding(12)
                .center_x(Length::Fill),
            );
        }
        if visible.is_empty() {
            if !self.loading.is_empty() {
                list = list.push(loading_box("Loading packages…"));
            } else {
                list = list.push(container(muted("Nothing matches your filters.".to_string())).padding(40).center_x(Length::Fill));
            }
        }

        let checked = self.checked_pkgs();
        let mut info = row![muted(format!("{} shown", visible.len())).size(12)].spacing(10).align_y(Alignment::Center);
        if any_updates {
            let free = self.busy.is_none() && self.pending.is_none();
            info = info.push(space::horizontal()).push(
                button(text("Select all updates").size(12))
                    .padding([4, 10])
                    .style(button::secondary)
                    .on_press(Message::CheckAllShown)
                    .tip("Tick every package shown here that has an update"),
            );
            if !checked.is_empty() {
                info = info.push(
                    button(text("Clear").size(12))
                        .padding([4, 10])
                        .style(button::secondary)
                        .on_press(Message::ClearChecked)
                        .tip("Untick all packages"),
                );
            }
            let action = advice::update_selected_action(&checked);
            let help = action
                .as_ref()
                .map(|a| format!("Install updates for the ticked packages. Runs: {}", advice::describe(&a.kind)))
                .unwrap_or_else(|| "Tick packages with the boxes on the left first".into());
            info = info.push(
                button(text(format!("Update selected ({})", checked.len())).size(12))
                    .padding([4, 10])
                    .style(button::success)
                    .on_press_maybe(action.filter(|_| free).map(Message::RunAction))
                    .tip(help),
            );
        }
        let mut col = column![
            toolbar,
            info,
            scrollable(container(list).padding(iced::Padding { right: 14.0, ..Default::default() }))
                .height(Length::Fill),
        ]
        .spacing(10);

        col = self.push_status(col);
        container(col).padding(18).width(Length::Fill).height(Length::Fill).into()
    }

    fn pkg_row<'a>(&'a self, p: &'a Package) -> Element<'a, Message> {
        let selected = self.selected.as_ref().is_some_and(|(s, id)| *s == p.source && *id == p.id);
        let mut badges = row![].spacing(6).align_y(Alignment::Center);
        if let Some(l) = p.telemetry {
            badges = badges.push(pill(format!("privacy: {}", l.label()), level_color(l)));
        }
        if p.new_version.is_some() {
            badges = badges.push(pill("update".into(), GREEN));
        }
        badges = badges.push(pill(p.source.short().into(), SLATE));

        let summary: String = p.summary.chars().take(110).collect();
        let content = row![
            column![
                text(&p.name).size(15).font(BOLD),
                muted(summary).size(12),
            ]
            .spacing(2)
            .width(Length::Fill),
            badges,
            column![
                text(short_version(&p.version)).size(12),
                muted(p.size_bytes.map(human_size).unwrap_or_default()).size(11),
                muted(p.installed_at.map(|t| format!("installed {}", model::format_date(t))).unwrap_or_default()).size(10),
            ]
            .align_x(Alignment::End)
            .width(Length::Fixed(130.0)),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        button(content)
            .width(Length::Fill)
            .padding([8, 12])
            .on_press(Message::Select((p.source, p.id.clone())))
            .style(move |t: &Theme, st| row_style(t, st, selected))
            .into()
    }

    fn detail_view(&self) -> Element<'_, Message> {
        let body: Element<Message> = match self.selected_pkg() {
            None => container(muted("Select a package to see details, dependents, updates and privacy information.".into()))
                .padding(30)
                .center(Length::Fill)
                .into(),
            Some(p) => self.detail_body(p),
        };
        container(body)
            .width(Length::Fixed(460.0))
            .height(Length::Fill)
            .style(|t: &Theme| container::Style {
                background: Some(t.extended_palette().background.weak.color.scale_alpha(0.35).into()),
                ..Default::default()
            })
            .into()
    }

    fn detail_body<'a>(&'a self, p: &'a Package) -> Element<'a, Message> {
        let busy = self.busy.is_some();
        let mut col = Column::new().spacing(14).padding(22);

        col = col.push(
            column![
                text(&p.name).size(24).font(BOLD),
                muted(p.id.clone()).size(12),
                row![
                    pill(p.source.label().into(), SLATE),
                    pill(format!("{:?}", p.kind).to_lowercase(), SLATE),
                    pill(short_version(&p.version), SLATE),
                ]
                .spacing(6),
            ]
            .spacing(6),
        );
        if !p.summary.is_empty() {
            col = col.push(text(&p.summary).size(14));
        }

        // Actions
        let mut actions = row![].spacing(10);
        let running = self.details.as_ref().is_some_and(|d| !d.running.is_empty());
        // snapd refuses to refresh a snap while it is running.
        let snap_blocked = p.source == Source::Snap && running;
        if let Some(nv) = &p.new_version {
            actions = actions.push(
                button(text(format!("Update to {}", short_version(nv))).size(14))
                    .padding([9, 16])
                    .style(button::success)
                    .on_press_maybe((!busy && !snap_blocked).then_some(Message::Update)).tip("Install the newer version (asks for your password)"),
            );
        } else if !self.checking.contains(&p.source) {
            actions = actions.push(container(muted("Up to date".into()).size(13)).padding([9, 0]));
        } else {
            actions = actions.push(container(muted("Checking for updates…".into()).size(13)).padding([9, 0]));
        }
        actions = actions.push(space::horizontal()).push(
            button(text("Uninstall").size(14))
                .padding([9, 16])
                .style(button::danger)
                .on_press_maybe((!busy && !self.confirm_remove).then_some(Message::AskRemove)).tip("Remove this package. You will see what else is affected and confirm first."),
        );
        col = col.push(actions);

        if p.protected {
            col = col.push(banner(
                text("This is an essential system package. Removing it can break your system.").size(13).into(),
                None,
                Level::High,
            ));
        }

        let Some(d) = &self.details else {
            return scrollable(col.push(loading_box("Analyzing…"))).into();
        };

        if snap_blocked && p.new_version.is_some() {
            col = col.push(banner(
                text(format!(
                    "Close {} to update it: snaps can't be updated while running. Snap also updates it automatically once it's closed.",
                    p.name
                ))
                .size(13)
                .into(),
                None,
                Level::Low,
            ));
        }
        let twins: Vec<&Package> = self
            .packages
            .iter()
            .filter(|o| o.source != p.source && (o.name.eq_ignore_ascii_case(&p.name) || o.id == p.id))
            .collect();
        col = col.push(verdict_card(&advice::verdict(p, d, twins.len())));
        for o in twins {
            col = col.push(banner(
                text(format!(
                    "Also installed from {} (version {}). You may have duplicate copies; keep the one you use.",
                    o.source.label(),
                    short_version(&o.version)
                ))
                .size(13)
                .into(),
                None,
                Level::Info,
            ));
        }

        if self.confirm_remove {
            let mut c = Column::new().spacing(8).push(text(format!("Remove {}?", p.name)).font(BOLD).size(15));
            if let Some(w) = &d.would_remove {
                if w.len() > 1 {
                    c = c.push(text(format!("This will also remove {} other package(s):", w.len() - 1)).size(13));
                    c = c.push(muted(list_preview(w.iter().filter(|x| **x != p.name && **x != p.id), 25)).size(12));
                }
            } else if !d.used_by.is_empty() {
                c = c.push(text(format!("{} installed package(s) depend on it and may stop working or be removed.", d.used_by.len())).size(13));
            }
            if !d.running.is_empty() {
                c = c.push(text(format!("It is running right now ({} process(es)).", d.running.len())).size(13));
            }
            c = c.push(
                row![
                    button(text("Cancel").size(13)).style(button::secondary).on_press(Message::CancelRemove).tip("Keep the package"),
                    button(text("Yes, uninstall").size(13)).style(button::danger).on_press(Message::ConfirmRemove).tip("Remove it now (asks for your password)"),
                ]
                .spacing(10),
            );
            col = col.push(card(c.into(), Some(RED)));
        }

        if let Some(a) = about_section(d) {
            col = col.push(a);
        }
        col = col.push(privacy_section(&d.telemetry));
        if !d.switches.is_empty() {
            col = col.push(self.switches_section(p, &d.switches));
        }

        // Usage
        col = col.push(list_section(format!("USED BY ({})", d.used_by.len()), &d.used_by, "No other packages depend on it, so removing it won't break other software. You may still use it directly, though."));
        let running: Vec<String> = d.running.iter().map(|(pid, c)| format!("[{pid}] {c}")).collect();
        col = col.push(list_section(format!("RUNNING NOW ({})", running.len()), &running, "Not running."));
        col = col.push(list_section(format!("DEPENDS ON ({})", d.depends_on.len()), &d.depends_on, "No dependencies."));
        col = col.push(list_section("LOCATION".into(), &d.location, "Unknown."));

        if !d.info.is_empty() || p.size_bytes.is_some() {
            let mut c = Column::new().spacing(4).push(section_title("DETAILS"));
            if let Some(s) = p.size_bytes {
                c = c.push(kv("Size", &human_size(s)));
            }
            for (k, v) in &d.info {
                c = c.push(kv(k, v));
            }
            col = col.push(c);
        }

        scrollable(col).height(Length::Fill).into()
    }

    // ------------------------------------------------------- install tab

    fn search_sidebar<'a>(&'a self, mut col: Column<'a, Message>) -> Element<'a, Message> {
        let count = |s: Option<Source>| self.results.iter().filter(|p| s.is_none_or(|s| p.source == s)).count();
        col = col.push(section_title("SEARCH IN")).push(nav_item(
            "All sources".into(),
            count(None).to_string(),
            self.search_source.is_none(),
            Message::SetSearchSource(None),
        ));
        for &s in &self.sources {
            let badge = if self.searching.contains(&s) {
                "searching…".to_string()
            } else if self.search_errors.contains_key(&s) {
                "error".to_string()
            } else {
                count(Some(s)).to_string()
            };
            col = col.push(nav_item(s.label().into(), badge, self.search_source == Some(s), Message::SetSearchSource(Some(s))));
        }
        col = col
            .push(Space::new().height(14))
            .push(muted("Tip: Flatpak and Snap apps run sandboxed. Native packages (deb/rpm/pacman) integrate best with the system.".into()).size(11))
            .push(Space::new().height(Length::Fill));
        for (s, e) in &self.search_errors {
            col = col.push(text(format!("{}: {}", s.short(), e)).size(11).style(text::danger));
        }
        self.sidebar_frame(col)
    }

    fn search_view(&self) -> Element<'_, Message> {
        let installed: HashSet<(Source, &str)> =
            self.packages.iter().flat_map(|p| [(p.source, p.id.as_str()), (p.source, p.name.as_str())]).collect();
        let toolbar = row![
            text_input("Search for software to install, e.g. vlc, gimp, firefox…", &self.query)
                .on_input(Message::Query)
                .on_submit(Message::RunSearch)
                .padding(10)
                .size(15),
            button(text("Search").size(14))
                .padding([10, 18])
                .style(button::primary)
                .on_press_maybe((self.query.trim().len() >= 2).then_some(Message::RunSearch)).tip("Search every package source (or press Enter)"),
            button(text("Install AppImage…").size(14))
                .padding([10, 14])
                .style(button::secondary)
                .on_press_maybe((!self.appimage_busy).then_some(Message::BrowseAppImage))
                .tip("Pick a downloaded .AppImage file and add it to your app menu like a regular app"),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let visible: Vec<&Package> =
            self.results.iter().filter(|p| self.search_source.is_none_or(|s| s == p.source)).collect();
        let mut list = Column::new().spacing(2);
        for p in &visible {
            let is_installed = installed.contains(&(p.source, p.id.as_str()));
            let selected = self.r_selected.as_ref().is_some_and(|(s, id)| *s == p.source && *id == p.id);
            let mut badges = row![].spacing(6).align_y(Alignment::Center);
            if let Some(l) = p.telemetry {
                badges = badges.push(pill(format!("privacy: {}", l.label()), level_color(l)));
            }
            if is_installed {
                badges = badges.push(pill("installed".into(), GREEN));
            }
            if p.source == Source::Snap && p.extra.contains("classic") {
                badges = badges.push(pill("classic".into(), ORANGE));
            }
            badges = badges.push(pill(p.source.short().into(), SLATE));
            let summary: String = p.summary.chars().take(110).collect();
            let content = row![
                column![text(&p.name).size(15).font(BOLD), muted(summary).size(12)].spacing(2).width(Length::Fill),
                badges,
                container(text(short_version(&p.version)).size(12)).align_right(Length::Fixed(110.0)),
            ]
            .spacing(12)
            .align_y(Alignment::Center);
            list = list.push(
                button(content)
                    .width(Length::Fill)
                    .padding([8, 12])
                    .on_press(Message::SelectResult((p.source, p.id.clone())))
                    .style(move |t: &Theme, st| row_style(t, st, selected)),
            );
        }
        if visible.is_empty() && !self.searching.is_empty() {
            list = list.push(loading_box("Searching repositories…"));
        } else if visible.is_empty() {
            let msg = if self.search_gen == 0 {
                "Type what you are looking for and press Enter."
            } else {
                "No results."
            };
            list = list.push(container(muted(msg.to_string())).padding(40).center_x(Length::Fill));
        }
        let shown = if self.searching.is_empty() {
            format!("{} results", visible.len())
        } else {
            format!("{} results so far…", visible.len())
        };
        let mut col = column![
            toolbar,
            muted(shown).size(12),
            scrollable(container(list).padding(iced::Padding { right: 14.0, ..Default::default() })).height(Length::Fill),
        ]
        .spacing(10);
        col = self.push_status(col);
        container(col).padding(18).width(Length::Fill).height(Length::Fill).into()
    }

    fn remote_detail_view(&self) -> Element<'_, Message> {
        let body: Element<Message> = match (&self.r_selected, &self.r_details) {
            _ if self.appimage.is_some() || self.appimage_busy => self.appimage_panel(),
            (None, _) => column![
                container(muted("Search and select a package to see what installing it involves.".into())).padding(30).center_x(Length::Fill),
                container(self.appimage_picker()).padding(22),
            ]
            .into(),
            (Some(_), None) => container(loading_box("Loading details…")).center(Length::Fill).into(),
            (Some(_), Some((p, d))) => self.remote_body(p, d),
        };
        container(body)
            .width(Length::Fixed(460.0))
            .height(Length::Fill)
            .style(|t: &Theme| container::Style {
                background: Some(t.extended_palette().background.weak.color.scale_alpha(0.35).into()),
                ..Default::default()
            })
            .into()
    }

    fn remote_body<'a>(&'a self, p: &'a Package, d: &'a Details) -> Element<'a, Message> {
        let installed = self.packages.iter().any(|x| x.source == p.source && (x.id == p.id || x.name == p.id));
        let mut col = Column::new().spacing(14).padding(22);
        let mut pills = row![pill(p.source.label().into(), SLATE)].spacing(6);
        if !p.version.is_empty() {
            pills = pills.push(pill(short_version(&p.version), SLATE));
        }
        if let Some(s) = p.size_bytes {
            pills = pills.push(pill(human_size(s), SLATE));
        }
        col = col.push(column![text(&p.name).size(24).font(BOLD), muted(p.id.clone()).size(12), pills].spacing(6));
        if !p.summary.is_empty() {
            col = col.push(text(&p.summary).size(14));
        }
        let action: Element<Message> = if installed {
            row![
                pill("Already installed".into(), GREEN),
                space::horizontal(),
                button(text("Manage in Installed").size(13)).style(button::secondary).on_press(Message::SetTab(Tab::Installed)).tip("Show this package in the Installed tab"),
            ]
            .align_y(Alignment::Center)
            .into()
        } else {
            button(text("Install").size(14))
                .padding([9, 22])
                .style(button::primary)
                .on_press_maybe(self.busy.is_none().then_some(Message::Install)).tip("Install it (asks for your password)")
                .into()
        };
        col = col.push(action);

        if p.source == Source::Snap && p.extra.contains("classic") {
            col = col.push(banner(
                text("Classic snap: it is not sandboxed and has the same access to your system as a regular program.").size(13).into(),
                None,
                Level::Medium,
            ));
        }
        if let Some(w) = &d.would_install {
            let extra: Vec<&String> = w.iter().filter(|x| **x != p.id).collect();
            if !extra.is_empty() {
                col = col.push(
                    column![
                        section_title(format!("ALSO INSTALLS ({})", extra.len())),
                        muted(list_preview(extra.into_iter(), 30)).size(12),
                    ]
                    .spacing(4),
                );
            }
        }
        if let Some(a) = about_section(d) {
            col = col.push(a);
        }
        col = col.push(privacy_section(&d.telemetry));
        col = col.push(list_section(format!("DEPENDS ON ({})", d.depends_on.len()), &d.depends_on, "No dependencies listed."));
        if !d.info.is_empty() {
            let mut c = Column::new().spacing(4).push(section_title("DETAILS"));
            for (k, v) in &d.info {
                c = c.push(kv(k, v));
            }
            col = col.push(c);
        }
        scrollable(col).height(Length::Fill).into()
    }

    fn push_status<'a>(&'a self, col: Column<'a, Message>) -> Column<'a, Message> {
        if let Some(a) = self.pending.as_ref().filter(|_| matches!(self.tab, Tab::Installed | Tab::Install)) {
            let c = column![
                text(a.confirm.clone().unwrap_or_default()).size(14).font(BOLD),
                muted(advice::describe(&a.kind)).size(12),
                row![
                    button(text("Cancel").size(13)).style(button::secondary).on_press(Message::CancelAction).tip("Don't do it"),
                    button(text(format!("Yes, {}", a.label.to_lowercase())).size(13))
                        .style(button::danger)
                        .on_press(Message::ConfirmAction).tip(format!("Run now: {}", advice::describe(&a.kind))),
                ]
                .spacing(10),
            ]
            .spacing(8);
            col.push(card(c.into(), Some(RED)))
        } else if let Some(b) = &self.busy {
            col.push(banner(text(b.clone()).size(14).into(), None, Level::Info))
        } else if let Some((ok, m)) = &self.status {
            col.push(banner(
                text(m.clone()).size(13).into(),
                Some(Message::DismissStatus),
                if *ok { Level::Info } else { Level::High },
            ))
        } else {
            col
        }
    }

    // ---------------------------------------------------- suggestions tab

    fn suggestions_view(&self) -> Element<'_, Message> {
        let header = row![
            column![
                text("Suggestions").size(24).font(BOLD),
                muted("Problems to fix, what to clean up, what's duplicated, what's safe to remove, and how to stop tracking.".into()).size(13),
            ]
            .spacing(2)
            .width(Length::Fill),
            button(text("Scan again").size(14))
                .padding([10, 16])
                .style(button::secondary)
                .on_press_maybe((!self.scanning && self.loading.is_empty()).then_some(Message::ScanSuggestions)).tip("Analyze your system again"),
        ]
        .align_y(Alignment::Center);

        let mut body = Column::new().spacing(14);
        match &self.suggestions {
            None => {
                let msg = if !self.loading.is_empty() { "Loading packages…" } else { "Analyzing your system… this takes a few seconds." };
                body = body.push(loading_box(msg));
            }
            Some(s) => {
                let active = s.telemetry.iter().filter(|g| g.finding.off != Some(true)).count();
                let freeable: u64 = s.cleanup.iter().filter_map(|c| c.size).sum();
                let n_clean: usize = s.cleanup.iter().map(|c| c.items.len()).sum();
                let n_unused = s.unused.len();
                let n_problems = s.issues.iter().filter(|i| i.severity != health::Severity::Info).count();
                let stat = |label: &'static str, value: String, color: Color, sec: Section| {
                    button(stat_card(label, value, color))
                        .padding(0)
                        .width(Length::Fill)
                        .style(|_: &Theme, _| button::Style::default())
                        .on_press(Message::SetSection(sec))
                        .tip("Click to show only this section")
                };
                body = body.push(
                    row![
                        stat(
                            "Problems to fix",
                            n_problems.to_string(),
                            if s.issues.iter().any(|i| i.severity == health::Severity::Problem) { RED } else if n_problems > 0 { ORANGE } else { GREEN },
                            Section::Problems
                        ),
                        stat("Apps you don't use", n_unused.to_string(), if n_unused > 0 { YELLOW } else { GREEN }, Section::Unused),
                        stat("Telemetry to review", active.to_string(), if active > 0 { ORANGE } else { GREEN }, Section::Telemetry),
                        stat("Apps installed twice", s.duplicates.len().to_string(), if s.duplicates.is_empty() { GREEN } else { BLUE }, Section::Duplicates),
                        stat("Leftovers to clean", n_clean.to_string(), if n_clean > 0 { YELLOW } else { GREEN }, Section::Cleanup),
                        stat("Space you can free", if freeable > 0 { human_size(freeable) } else { "—".into() }, GREEN, Section::Cleanup),
                    ]
                    .spacing(12),
                );
                // Section switcher.
                let chip = |label: String, sec: Section| {
                    let active = self.section == sec;
                    button(text(label).size(13))
                        .padding([6, 12])
                        .style(if active { button::primary } else { button::secondary })
                        .on_press(Message::SetSection(sec))
                };
                body = body.push(
                    row![
                        chip("All".into(), Section::All).tip("Show every section"),
                        chip(format!("Problems ({})", s.issues.len()), Section::Problems).tip("Broken installs, drivers, failed services, disk space"),
                        chip(format!("Apps you don't use ({n_unused})"), Section::Unused).tip("Apps not used for over 3 months"),
                        chip(format!("Privacy & telemetry ({})", s.telemetry.len()), Section::Telemetry).tip("Trackers: uninstall or turn off"),
                        chip(format!("Duplicates ({})", s.duplicates.len()), Section::Duplicates).tip("The same app installed from two sources"),
                        chip(format!("Cleanup ({})", s.cleanup.len()), Section::Cleanup).tip("Caches, logs, leftovers and old versions"),
                    ]
                    .spacing(8),
                );
                let show = |sec: Section| self.section == Section::All || self.section == sec;

                if show(Section::Problems) && (!s.issues.is_empty() || self.section == Section::Problems) {
                    body = body.push(section_header("PROBLEMS", "Things that break updates or your system, and how to fix them."));
                    if s.issues.is_empty() {
                        body = body.push(muted("No problems found.".into()).size(13));
                    }
                    for i in &s.issues {
                        let confirm = i.actions.iter().find_map(|a| self.confirm_box(a));
                        body = body.push(issue_card(i, self.busy.is_some(), confirm));
                    }
                }
                if show(Section::Unused) && !s.unused.is_empty() {
                    let total: u64 = s.unused.iter().filter_map(|u| u.size).sum();
                    body = body.push(section_header(
                        "APPS YOU DON'T USE",
                        "Not used for over 3 months (or never started), biggest first.",
                    ));
                    body = body.push(muted(format!("Removing all of them would free about {}.", human_size(total))).size(12));
                    let mut c = Column::new().spacing(8);
                    for u in &s.unused {
                        let extra: Element<Message> = row![
                            pill(u.when.clone(), YELLOW),
                            muted(u.size.map(human_size).unwrap_or_default()).size(12),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .into();
                        c = c.push(self.member_row(&u.member, true, Some(extra)));
                    }
                    body = body.push(card(c.into(), Some(YELLOW)));
                }
                if self.section == Section::Unused && s.unused.is_empty() {
                    body = body.push(section_header("APPS YOU DON'T USE", "Not used for over 3 months (or never started)."));
                    body = body.push(muted("Every app was used recently.".into()).size(13));
                }
                if show(Section::Telemetry) {
                    body = body.push(section_header(
                        "PRIVACY & TELEMETRY",
                        "For each tracker: remove it if nothing needs it, otherwise turn it off.",
                    ));
                    if s.telemetry.is_empty() {
                        body = body.push(muted("No known telemetry found.".into()).size(13));
                    }
                    for g in &s.telemetry {
                        body = body.push(self.telemetry_card(g));
                    }
                }
                if show(Section::Duplicates) {
                    body = body.push(section_header(
                        "DUPLICATE APPS",
                        "The same app installed from more than one source. You usually only need one.",
                    ));
                    if s.duplicates.is_empty() {
                        body = body.push(muted("No duplicates found.".into()).size(13));
                    }
                    for g in &s.duplicates {
                        body = body.push(self.dup_card(g));
                    }
                }
                if show(Section::Cleanup) {
                    body = body.push(section_header("CLEANUP", "Leftovers that nothing uses any more."));
                    if s.cleanup.is_empty() {
                        body = body.push(muted("Nothing to clean up.".into()).size(13));
                    }
                    for c in &s.cleanup {
                        let confirm = c.action.as_ref().and_then(|a| self.confirm_box(a));
                        body = body.push(cleanup_card(c, self.busy.is_some(), confirm));
                    }
                }
                body = body.push(
                    muted("Removal verdicts come from your package manager's dry runs and dependency data. Telemetry detection is a curated heuristic, not a network audit.".into())
                        .size(11),
                );
            }
        }
        let col = self
            .push_status(column![header].spacing(14))
            .push(
                scrollable(container(body).padding(iced::Padding { right: 14.0, ..Default::default() }))
                    .id(SUGG_SCROLL)
                    .height(Length::Fill),
            );
        container(col).padding(22).width(Length::Fill).height(Length::Fill).into()
    }

    fn member_row<'a>(&'a self, m: &'a advice::Member, show_remove: bool, extra: Option<Element<'a, Message>>) -> Element<'a, Message> {
        let busy = self.busy.is_some();
        let mut r = row![
            button(text(&m.pkg.name).size(14).font(BOLD))
                .padding(0)
                .style(link_style)
                .on_press(Message::ShowPopup((m.pkg.source, m.pkg.id.clone())))
                .tip("Show details"),
            pill(m.pkg.source.short().into(), SLATE),
            muted(short_version(&m.pkg.version)).size(12),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(e) = extra {
            r = r.push(e);
        }
        if m.running {
            r = r.push(pill("running".into(), BLUE));
        }
        r = r.push(pill(m.verdict.risk.label().into(), risk_color(m.verdict.risk)));
        r = r.push(space::horizontal());
        r = r.push(
            button(text("Details").size(12))
                .style(button::text)
                .on_press(Message::OpenPackage((m.pkg.source, m.pkg.id.clone()))).tip("Open this package in the Installed tab"),
        );
        let mut confirm = None;
        if show_remove && m.verdict.risk != advice::Risk::Danger {
            let mut q = format!("Uninstall {} ({})?", m.pkg.name, m.pkg.source.label());
            if !m.verdict.reasons.is_empty() {
                q.push(' ');
                q.push_str(&m.verdict.reasons.join(" "));
            }
            let action = advice::Action {
                label: "Uninstall".into(),
                kind: advice::ActionKind::Remove(vec![m.pkg.clone()]),
                confirm: Some(q),
            };
            confirm = self.confirm_box(&action);
            r = r.push(
                button(text("Uninstall").size(12))
                    .style(button::danger)
                    .on_press_maybe((!busy && confirm.is_none()).then(|| Message::RunAction(action)))
                    .tip("Remove this package (you confirm first)"),
            );
        }
        let mut c = Column::new().spacing(2).push(r);
        if let Some(b) = confirm {
            c = c.push(b);
        }
        if m.verdict.risk != advice::Risk::Safe {
            if let Some(reason) = m.verdict.reasons.first() {
                c = c.push(muted(reason.clone()).size(11));
            }
        }
        if !m.note.is_empty() {
            c = c.push(muted(m.note.clone()).size(11));
        }
        c.into()
    }

    /// Inline "are you sure?" box, shown right where the action was clicked.
    fn confirm_box<'a>(&self, a: &advice::Action) -> Option<Element<'a, Message>> {
        let p = self.pending.as_ref()?;
        if p.label != a.label || advice::describe(&p.kind) != advice::describe(&a.kind) {
            return None;
        }
        let c = column![
            text(p.confirm.clone().unwrap_or_default()).size(13).font(BOLD),
            muted(advice::describe(&p.kind)).size(11),
            row![
                button(text("Cancel").size(13)).style(button::secondary).on_press(Message::CancelAction).tip("Don't do it"),
                button(text(format!("Yes, {}", p.label.to_lowercase())).size(13))
                    .style(button::danger)
                    .on_press(Message::ConfirmAction).tip(format!("Run now: {}", advice::describe(&p.kind))),
            ]
            .spacing(10),
        ]
        .spacing(6);
        Some(
            container(c)
                .padding(10)
                .width(Length::Fill)
                .style(|_| container::Style {
                    background: Some(RED.scale_alpha(0.12).into()),
                    border: Border { radius: 8.0.into(), width: 1.0, color: RED.scale_alpha(0.6) },
                    ..Default::default()
                })
                .into(),
        )
    }

    fn telemetry_card<'a>(&'a self, g: &'a advice::TelemetryGroup) -> Element<'a, Message> {
        let f = &g.finding;
        let off = f.off == Some(true);
        let mut title = row![pill(f.level.label().into(), level_color(f.level)), text(&f.title).size(15).font(BOLD)]
            .spacing(8)
            .align_y(Alignment::Center);
        if off {
            title = title.push(pill("Already turned off".into(), GREEN));
        }
        let mut c = Column::new()
            .spacing(8)
            .push(title)
            .push(text(&f.detail).size(12))
            .push(text(format!("Suggestion: {}", g.advice)).size(13).font(BOLD));
        let can_remove = g.recommend_remove
            || telemetry::is_system_component(&f.title)
            || !g.members.iter().any(|m| m.pkg.kind == Kind::App);
        for m in &g.members {
            c = c.push(self.member_row(m, can_remove && !off, None));
        }
        if !off {
            if let Some(a) = &g.fix {
                c = c.push(
                    row![
                        button(text(a.label.clone()).size(13))
                            .padding([7, 14])
                            .style(if g.recommend_remove { button::secondary } else { button::primary })
                            .on_press_maybe(self.busy.is_none().then(|| Message::RunAction(a.clone()))).tip(advice::describe(&a.kind)),
                        muted(advice::describe(&a.kind)).size(11),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
            if let Some(dis) = &f.disable {
                c = c.push(muted(format!("Manually: {dis}")).size(12));
            }
        }
        card(c.into(), Some(if off { GREEN } else { level_color(f.level) }))
    }

    fn dup_card<'a>(&'a self, g: &'a advice::DupGroup) -> Element<'a, Message> {
        let mut c = Column::new()
            .spacing(8)
            .push(
                row![text(&g.name).size(15).font(BOLD), muted(format!("installed {} times", g.members.len())).size(12)]
                    .spacing(8)
                    .align_y(Alignment::Center),
            )
            .push(text(format!("Suggestion: {}", g.advice)).size(13).font(BOLD));
        for m in &g.members {
            let tag: Element<Message> = if m.keep {
                pill("keep".into(), GREEN)
            } else if m.note.is_empty() {
                pill("can remove".into(), ORANGE)
            } else {
                pill("pointer".into(), SLATE)
            };
            c = c.push(self.member_row(m, !m.keep && m.note.is_empty(), Some(tag)));
        }
        card(c.into(), Some(BLUE))
    }
}

/// Hover tooltip for any widget.
trait Tip<'a> {
    fn tip(self, label: impl Into<String>) -> Element<'a, Message>;
}

impl<'a, T: Into<Element<'a, Message>>> Tip<'a> for T {
    fn tip(self, label: impl Into<String>) -> Element<'a, Message> {
        let bubble = container(text(label.into()).size(12)).padding([5, 9]).max_width(340).style(|t: &Theme| {
            let pal = t.extended_palette();
            container::Style {
                background: Some(pal.background.strong.color.into()),
                text_color: Some(pal.background.strong.text),
                border: Border { radius: 6.0.into(), width: 1.0, color: pal.background.base.text.scale_alpha(0.15) },
                ..Default::default()
            }
        });
        tooltip(self, bubble, tooltip::Position::Bottom).gap(6).snap_within_viewport(true).into()
    }
}

fn link_style(t: &Theme, st: button::Status) -> button::Style {
    let pal = t.extended_palette();
    button::Style {
        background: None,
        text_color: if matches!(st, button::Status::Hovered) { pal.primary.strong.color } else { pal.background.base.text },
        ..Default::default()
    }
}

/// Dim the page and show `content` centred on top; clicking outside closes it.
fn modal<'a>(base: Element<'a, Message>, content: Element<'a, Message>, on_blur: Message) -> Element<'a, Message> {
    stack![
        base,
        opaque(
            mouse_area(center(opaque(content)).style(|_| container::Style {
                background: Some(Color { a: 0.55, ..Color::BLACK }.into()),
                ..Default::default()
            }))
            .on_press(on_blur)
        )
    ]
    .into()
}

/// Centered loading animation with a caption.
fn loading_box<'a>(msg: &'a str) -> Element<'a, Message> {
    container(column![spinner::spinner(120.0), muted(msg.to_string()).size(13)].spacing(6).align_x(Alignment::Center))
        .padding(30)
        .center_x(Length::Fill)
        .into()
}

/// "What is this app?" text plus a website button.
fn about_section(d: &Details) -> Option<Element<'_, Message>> {
    if d.description.is_empty() && d.homepage.is_none() {
        return None;
    }
    let mut c = Column::new().spacing(6).push(section_title("ABOUT"));
    if !d.description.is_empty() {
        let mut text_: String = d.description.chars().take(1500).collect();
        if d.description.chars().count() > 1500 {
            text_.push('…');
        }
        c = c.push(text(text_).size(13));
    }
    if let Some(url) = &d.homepage {
        c = c.push(
            button(text("Visit website").size(12))
                .style(button::secondary)
                .on_press(Message::OpenUrl(url.clone()))
                .tip(format!("Open {url} in your browser")),
        );
    }
    Some(c.into())
}

fn risk_color(r: advice::Risk) -> Color {
    match r {
        advice::Risk::Safe => GREEN,
        advice::Risk::Caution => ORANGE,
        advice::Risk::Danger => RED,
    }
}

fn verdict_card<'a>(v: &advice::Verdict) -> Element<'a, Message> {
    let color = risk_color(v.risk);
    let mut c = Column::new()
        .spacing(4)
        .push(row![pill(v.risk.label().into(), color), text(v.headline.clone()).size(13)].spacing(8).align_y(Alignment::Center));
    for r in &v.reasons {
        c = c.push(muted(format!("• {r}")).size(12));
    }
    card(c.into(), Some(color))
}

fn stat_card<'a>(label: &'a str, value: String, color: Color) -> Element<'a, Message> {
    card(
        column![text(value).size(26).font(BOLD).color(color), muted(label.to_string()).size(12)].spacing(2).into(),
        None,
    )
}

fn section_header<'a>(title: &'a str, sub: &'a str) -> Element<'a, Message> {
    column![Space::new().height(6), section_title(title), muted(sub.to_string()).size(12)].spacing(2).into()
}

fn severity_color(s: health::Severity) -> Color {
    match s {
        health::Severity::Problem => RED,
        health::Severity::Warning => ORANGE,
        health::Severity::Info => BLUE,
    }
}

fn issue_card<'a>(i: &'a health::Issue, busy: bool, confirm: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let head = row![text(&i.title).size(15).font(BOLD), pill(i.severity.label().into(), severity_color(i.severity))]
        .spacing(8)
        .align_y(Alignment::Center);
    let mut col = Column::new().spacing(6).push(head).push(text(&i.detail).size(12));
    if !i.items.is_empty() {
        let mut lines: Vec<&str> = i.items.iter().take(12).map(String::as_str).collect();
        let more = i.items.len().saturating_sub(12);
        let tail = format!("… and {more} more");
        if more > 0 {
            lines.push(&tail);
        }
        col = col.push(muted(lines.join("\n")).size(11).font(Font::MONOSPACE));
    }
    if !i.actions.is_empty() {
        let mut buttons = row![].spacing(8);
        for (n, a) in i.actions.iter().enumerate() {
            buttons = buttons.push(
                button(text(a.label.clone()).size(13))
                    .padding([7, 14])
                    .style(if n == 0 { button::primary } else { button::secondary })
                    .on_press_maybe((!busy && confirm.is_none()).then(|| Message::RunAction(a.clone())))
                    .tip(advice::describe(&a.kind)),
            );
        }
        col = col.push(buttons);
    }
    if let Some(b) = confirm {
        col = col.push(b);
    }
    card(col.into(), Some(severity_color(i.severity)))
}

fn cleanup_card<'a>(c: &'a advice::Cleanup, busy: bool, confirm: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let mut head = row![text(&c.title).size(15).font(BOLD), pill(c.risk.label().into(), risk_color(c.risk))]
        .spacing(8)
        .align_y(Alignment::Center);
    if let Some(s) = c.size {
        head = head.push(pill(human_size(s), SLATE));
    }
    head = head.push(pill(format!("{} item(s)", c.items.len()), SLATE));
    let mut col = Column::new()
        .spacing(6)
        .push(head)
        .push(text(&c.detail).size(12))
        .push(muted(list_preview(c.items.iter(), 12)).size(11).font(Font::MONOSPACE));
    if let Some(a) = &c.action {
        col = col.push(
            button(text(a.label.clone()).size(13))
                .padding([7, 14])
                .style(button::primary)
                .on_press_maybe((!busy && confirm.is_none()).then(|| Message::RunAction(a.clone()))).tip(advice::describe(&a.kind)),
        );
    }
    if let Some(b) = confirm {
        col = col.push(b);
    }
    card(col.into(), Some(risk_color(c.risk)))
}

// ------------------------------------------------------------- helpers

const GREEN: Color = Color::from_rgb(0.40, 0.78, 0.47);
const RED: Color = Color::from_rgb(0.93, 0.38, 0.45);
const ORANGE: Color = Color::from_rgb(0.98, 0.63, 0.35);
const YELLOW: Color = Color::from_rgb(0.90, 0.78, 0.35);
const BLUE: Color = Color::from_rgb(0.45, 0.65, 0.98);
const SLATE: Color = Color::from_rgb(0.55, 0.58, 0.68);


fn row_style(t: &Theme, st: button::Status, selected: bool) -> button::Style {
    let pal = t.extended_palette();
    let bg = if selected {
        pal.primary.weak.color.scale_alpha(0.35)
    } else if matches!(st, button::Status::Hovered) {
        pal.background.weak.color
    } else {
        Color::TRANSPARENT
    };
    button::Style {
        background: Some(bg.into()),
        text_color: pal.background.base.text,
        border: Border { radius: 8.0.into(), ..Default::default() },
        ..Default::default()
    }
}

fn tab_button<'a>(label: &'a str, active: bool, msg: Message) -> Element<'a, Message> {
    button(container(text(label).size(13).font(BOLD)).center_x(Length::Fill))
        .width(Length::Fill)
        .padding([8, 6])
        .on_press(msg)
        .style(move |t: &Theme, st| {
            let pal = t.extended_palette();
            let (bg, fg) = if active {
                (pal.primary.strong.color, pal.primary.strong.text)
            } else if matches!(st, button::Status::Hovered) {
                (pal.background.strong.color, pal.background.base.text)
            } else {
                (pal.background.weak.color, pal.background.base.text)
            };
            button::Style {
                background: Some(bg.into()),
                text_color: fg,
                border: Border { radius: 8.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

fn privacy_section<'a>(findings: &'a [telemetry::Finding]) -> Element<'a, Message> {
    let mut priv_col = Column::new().spacing(8).push(section_title("PRIVACY & TELEMETRY"));
    if findings.is_empty() {
        priv_col = priv_col.push(text("No known telemetry for this package.").size(13)).push(
            muted("Based on a curated database plus dependency and sandbox checks. This is a heuristic, not a guarantee.".into()).size(11),
        );
    }
    for f in findings {
        let mut c = Column::new()
            .spacing(4)
            .push(row![pill(f.level.label().into(), level_color(f.level)), text(&f.title).size(14).font(BOLD)].spacing(8).align_y(Alignment::Center))
            .push(text(&f.detail).size(12));
        if f.off == Some(true) {
            c = c.push(pill("Already turned off".into(), GREEN));
        } else if let Some(fix) = f.fix {
            let a = advice::fix_action(fix);
            c = c.push(
                row![
                    button(text(a.label.clone()).size(12)).style(button::primary).on_press(Message::RunAction(a.clone())).tip(advice::describe(&a.kind)),
                    muted(advice::describe(&a.kind)).size(11),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        if let Some(dis) = &f.disable {
            c = c.push(muted(format!("Manually: {dis}")).size(12));
        }
        priv_col = priv_col.push(card(c.into(), Some(level_color(f.level))));
    }
    priv_col.into()
}

fn level_color(l: Level) -> Color {
    match l {
        Level::High => RED,
        Level::Medium => ORANGE,
        Level::Low => YELLOW,
        Level::Info => BLUE,
    }
}

fn muted<'a>(s: String) -> text::Text<'a> {
    text(s).style(|t: &Theme| text::Style {
        color: Some(t.extended_palette().background.base.text.scale_alpha(0.6)),
    })
}

fn section_title<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(11).font(BOLD).style(|t: &Theme| text::Style {
        color: Some(t.extended_palette().background.base.text.scale_alpha(0.5)),
    })
}

fn pill<'a>(label: String, color: Color) -> Element<'a, Message> {
    container(text(label).size(11).color(color))
        .padding([2, 8])
        .style(move |_| container::Style {
            background: Some(color.scale_alpha(0.15).into()),
            border: Border { radius: 10.0.into(), width: 1.0, color: color.scale_alpha(0.4) },
            ..Default::default()
        })
        .into()
}

fn card<'a>(content: Element<'a, Message>, accent: Option<Color>) -> Element<'a, Message> {
    container(content)
        .padding(12)
        .width(Length::Fill)
        .style(move |t: &Theme| {
            let pal = t.extended_palette();
            container::Style {
                background: Some(pal.background.base.color.into()),
                border: Border {
                    radius: 10.0.into(),
                    width: 1.0,
                    color: accent.map(|c| c.scale_alpha(0.5)).unwrap_or(pal.background.strong.color),
                },
                ..Default::default()
            }
        })
        .into()
}

fn banner<'a>(content: Element<'a, Message>, dismiss: Option<Message>, level: Level) -> Element<'a, Message> {
    let mut r = row![container(content).width(Length::Fill)].align_y(Alignment::Center).spacing(10);
    if let Some(m) = dismiss {
        r = r.push(button(text("Dismiss").size(12)).style(button::text).on_press(m).tip("Hide this message"));
    }
    card(r.into(), Some(level_color(level)))
}

fn nav_item<'a>(label: String, badge: String, active: bool, msg: Message) -> Element<'a, Message> {
    button(row![text(label).size(14), space::horizontal(), text(badge).size(12)].align_y(Alignment::Center))
        .width(Length::Fill)
        .padding([7, 10])
        .on_press(msg)
        .style(move |t: &Theme, st| {
            let pal = t.extended_palette();
            let (bg, fg) = if active {
                (pal.primary.base.color, pal.primary.base.text)
            } else if matches!(st, button::Status::Hovered) {
                (pal.background.strong.color.scale_alpha(0.5), pal.background.base.text)
            } else {
                (Color::TRANSPARENT, pal.background.base.text)
            };
            button::Style {
                background: Some(bg.into()),
                text_color: fg,
                border: Border { radius: 8.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

fn kv<'a>(k: &str, v: &str) -> Element<'a, Message> {
    row![
        muted(k.to_string()).size(12).width(Length::Fixed(130.0)),
        text(v.to_string()).size(12).width(Length::Fill),
    ]
    .spacing(8)
    .into()
}

fn list_preview<'a>(items: impl Iterator<Item = &'a String>, max: usize) -> String {
    let all: Vec<&String> = items.collect();
    let mut s = all.iter().take(max).map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    if all.len() > max {
        s.push_str(&format!(" … and {} more", all.len() - max));
    }
    s
}

fn list_section<'a>(title: String, items: &[String], empty: &str) -> Element<'a, Message> {
    let mut c = Column::new().spacing(4).push(section_title(title));
    if items.is_empty() {
        c = c.push(muted(empty.to_string()).size(12));
    } else {
        for i in items.iter().take(30) {
            c = c.push(text(i.clone()).size(12).font(Font::MONOSPACE));
        }
        if items.len() > 30 {
            c = c.push(muted(format!("… and {} more", items.len() - 30)).size(12));
        }
    }
    c.into()
}

fn short_version(v: &str) -> String {
    if v.chars().count() > 22 {
        format!("{}…", v.chars().take(21).collect::<String>())
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disclaimer_shows_once() {
        let _guard = crate::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var_os("HOME");
        let home = std::env::temp_dir().join(format!("pkgtrack-disclaimer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let (mut app, _) = App::new();
        assert!(app.disclaimer, "shown on first launch");
        let _ = app.view();
        let _ = app.update(Message::Tick);
        assert!(app.disclaimer, "clicking outside doesn't dismiss it");
        let _ = app.update(Message::AcceptDisclaimer);
        assert!(!app.disclaimer && app.status.is_none());
        let (again, _) = App::new();
        assert!(!again.disclaimer, "not shown after accepting");
        if let Some(h) = old_home {
            unsafe { std::env::set_var("HOME", h) };
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn uninstall_from_suggestions_asks_inline() {
        let (mut app, _) = App::new();
        app.tab = Tab::Suggestions;
        let pkg = Package {
            id: "dotnet-targeting-pack-8.0".into(),
            name: "dotnet-targeting-pack-8.0".into(),
            version: "8.0".into(),
            source: Source::Apt,
            summary: String::new(),
            size_bytes: None,
            kind: Kind::Library,
            protected: false,
            extra: String::new(),
            new_version: None,
            telemetry: None,
            installed_at: None,
        };
        let action = advice::Action {
            label: "Uninstall".into(),
            kind: advice::ActionKind::Remove(vec![pkg]),
            confirm: Some("Uninstall?".into()),
        };
        let _ = app.update(Message::RunAction(action.clone()));
        assert!(app.pending.is_some(), "click must open a confirmation");
        assert!(app.busy.is_none(), "nothing runs before confirming");
        assert!(app.confirm_box(&action).is_some(), "confirmation is shown next to the button");
        let _ = app.update(Message::CancelAction);
        assert!(app.pending.is_none());
    }

    #[test]
    fn app_update_popup_shows_and_closes() {
        let (mut app, _) = App::new();
        let rel = update::Release { version: "9.9.9".into(), url: "https://example.com".into(), notes: "- New".into() };
        let _ = app.update(Message::AppUpdateChecked(Ok(Some(rel))));
        assert!(app.app_update.is_some());
        let _ = app.view();
        let _ = app.update(Message::CloseAppUpdate);
        assert!(app.app_update.is_none());
        let _ = app.update(Message::AppUpdateChecked(Err("offline".into())));
        assert!(app.app_update.is_none() && app.status.is_none(), "offline must stay silent");
    }

    #[test]
    fn details_popup_opens_and_closes() {
        let (mut app, _) = App::new();
        app.tab = Tab::Suggestions;
        let pkg = Package {
            id: "viber".into(),
            name: "viber".into(),
            version: "1.0".into(),
            source: Source::Apt,
            summary: "Messenger".into(),
            size_bytes: Some(500_000_000),
            kind: Kind::App,
            protected: false,
            extra: String::new(),
            new_version: None,
            telemetry: None,
            installed_at: None,
        };
        app.packages.push(pkg.clone());
        let key = (Source::Apt, "viber".to_string());
        let _ = app.update(Message::ShowPopup(key.clone()));
        assert_eq!(app.popup.as_ref(), Some(&key));
        let _ = app.view(); // loading state renders
        let d = Details { info: vec![("Last opened".into(), "10 months ago".into())], ..Default::default() };
        let _ = app.update(Message::PopupLoaded(key.clone(), Box::new(d)));
        assert!(app.popup_details.is_some());
        let _ = app.view(); // full popup renders
        // Uninstall inside the popup asks first.
        let action = advice::Action {
            label: "Uninstall".into(),
            kind: advice::ActionKind::Remove(vec![pkg]),
            confirm: Some("Uninstall viber (APT / dpkg)?".into()),
        };
        let _ = app.update(Message::RunAction(action.clone()));
        assert!(app.pending.is_some() && app.busy.is_none());
        let _ = app.view();
        let _ = app.update(Message::ClosePopup);
        assert!(app.popup.is_none() && app.pending.is_none());
        // While something runs, the loading overlay is drawn and animation ticks are requested.
        app.busy = Some("Deleting caches…".into());
        assert!(app.animating());
        let _ = app.view();
        app.busy = None;
        app.suggestions = Some(advice::Suggestions::default());
        assert!(!app.animating(), "no redraw loop when idle");
        for sec in [Section::Problems, Section::Duplicates, Section::Unused, Section::Cleanup, Section::All] {
            let _ = app.update(Message::SetSection(sec));
            assert_eq!(app.section, sec);
            let _ = app.view();
        }
    }
}
