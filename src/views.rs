//! Views for the Network, Startup, Sources and History tabs.
use crate::advice::{self, Action, ActionKind};
use crate::backends::flatpak::Switch;
use crate::history::Op;
use crate::startup::{Hint, Kind as SKind};
use crate::telemetry::Level;
use crate::{
    Tip, card, level_color, list_preview, muted, pill, section_header, section_title, short_version, stat_card, App,
    Message, Tab, BLUE, BOLD, GREEN, ORANGE, RED, SLATE, YELLOW,
};
use iced::widget::{button, checkbox, column, container, horizontal_space, row, scrollable, text, text_input, toggler, Column, Space};
use iced::{Alignment, Element, Font, Length};

fn page<'a>(app: &'a App, title: &'a str, subtitle: &'a str, right: Option<Element<'a, Message>>, body: Column<'a, Message>) -> Element<'a, Message> {
    let mut header = row![column![text(title).size(24).font(BOLD), muted(subtitle.to_string()).size(13)].spacing(2).width(Length::Fill)]
        .align_y(Alignment::Center);
    if let Some(r) = right {
        header = header.push(r);
    }
    let col = app
        .push_status(column![header].spacing(14))
        .push(scrollable(container(body.spacing(12)).padding(iced::Padding { right: 14.0, ..Default::default() })).height(Length::Fill));
    container(col).padding(22).width(Length::Fill).height(Length::Fill).into()
}

fn loading<'a>(msg: &'a str) -> Column<'a, Message> {
    Column::new().push(crate::loading_box(msg))
}

impl App {
    pub(crate) fn extra_sidebar(&self) -> Option<Element<'_, Message>> {
        let col: Column<Message> = match self.tab {
            Tab::Network => column![
                checkbox("Refresh every 3 seconds", self.net_auto).on_toggle(Message::SetNetAuto).size(16).text_size(14).tip("Update the list automatically while this tab is open"),
                checkbox("Only flagged connections", self.net_flagged).on_toggle(Message::SetNetFlagged).size(16).text_size(14).tip("Hide everything except connections to tracking, analytics and crash-report servers"),
                Space::with_height(10),
                muted("Shows which programs are talking to the internet right now. Names come from reverse DNS, so big services may show their cloud provider (e.g. 1e100.net = Google).".into()).size(12),
                muted("Connections of system services appear without a program name unless you run the app as root.".into()).size(12),
            ],
            Tab::Startup => column![
                checkbox("Only show suggestions", self.startup_hints_only).on_toggle(Message::SetStartupHints).size(16).text_size(14).tip("Hide items marked 'Keep on' and unknown ones"),
                Space::with_height(10),
                muted("Services and apps that start by themselves. Turning off what you don't need saves memory, speeds up boot and stops some background reporting.".into()).size(12),
                muted("Login apps are changed for your user only. System services ask for your password.".into()).size(12),
            ],
            Tab::Sources => column![
                muted("Every repository can install and update software on your system as root. Only keep the ones you trust and still use.".into()).size(12),
                muted("After enabling or disabling a repository, refresh the package lists.".into()).size(12),
            ],
            Tab::History => column![
                text_input("Filter by package or command…", &self.history_query).on_input(Message::HistoryQuery).padding(8).size(13),
                Space::with_height(10),
                section_title("MOVE TO A NEW PC"),
                button(text("Export app list…").size(13)).width(Length::Fill).style(button::secondary).on_press_maybe(self.loading.is_empty().then_some(Message::Export)).tip("Save the apps you installed to a text file, to reinstall them on another computer"),
                button(text("Import app list…").size(13)).width(Length::Fill).style(button::secondary).on_press_maybe(self.loading.is_empty().then_some(Message::ImportPick)).tip("Pick an exported app list and install whatever is missing on this computer"),
                muted("Export saves the apps you installed yourself to a text file. Import on another computer installs whatever is missing.".into()).size(12),
            ],
            Tab::Suggestions => column![
                muted("Suggestions look at your whole system: apps you don't use, telemetry you can turn off, apps installed twice, and leftovers that are safe to clean up.".into()).size(12),
                muted("Every removal shows what it affects and asks before doing anything.".into()).size(12),
            ],
            _ => return None,
        };
        Some(col.spacing(8).into())
    }

    // ------------------------------------------------------------- network

    pub(crate) fn network_view(&self) -> Element<'_, Message> {
        let refresh: Element<Message> = button(text("Refresh").size(14))
            .padding([10, 16])
            .style(button::secondary)
            .on_press_maybe((!self.net_busy).then_some(Message::NetTick)).tip("Read the current connections again")
            .into();
        let Some(snap) = &self.net else {
            return page(self, "Network", "Live connections per program", Some(refresh), loading("Reading connections…"));
        };
        let flagged_n: usize = snap.procs.iter().flat_map(|p| &p.conns).filter(|c| c.flag.as_ref().is_some_and(|f| f.1 > Level::Info)).count();
        let total: usize = snap.procs.iter().map(|p| p.conns.len()).sum();
        let public = snap.listeners.iter().filter(|l| l.public).count();
        let mut body = Column::new().push(
            row![
                stat_card("Programs online", snap.procs.len().to_string(), BLUE),
                stat_card("Connections", total.to_string(), BLUE),
                stat_card("To tracking / analytics", flagged_n.to_string(), if flagged_n > 0 { ORANGE } else { GREEN }),
                stat_card("Open to your network", public.to_string(), if public > 0 { YELLOW } else { GREEN }),
            ]
            .spacing(12),
        );
        body = body.push(section_header("CONNECTED PROGRAMS", "Outgoing connections, busiest first. Tracking and analytics servers are flagged."));
        let mut shown = 0;
        for p in &snap.procs {
            let conns: Vec<_> = p
                .conns
                .iter()
                .filter(|c| !self.net_flagged || c.flag.as_ref().is_some_and(|f| f.1 > Level::Info))
                .collect();
            if conns.is_empty() {
                continue;
            }
            shown += 1;
            let mut head = row![text(&p.name).size(15).font(BOLD)].spacing(8).align_y(Alignment::Center);
            if let Some(pkg) = &p.package {
                head = head.push(pill(pkg.clone(), SLATE));
            }
            if let Some(l) = self
                .packages
                .iter()
                .find(|x| p.package.as_deref().is_some_and(|pk| pk.split(' ').next() == Some(x.id.as_str())))
                .and_then(|x| x.telemetry)
            {
                head = head.push(pill(format!("known telemetry: {}", l.label()), level_color(l)));
            }
            head = head.push(horizontal_space()).push(muted(format!("{} connection(s)", conns.len())).size(12));
            let mut c = Column::new().spacing(4).push(head);
            if !p.exe.is_empty() {
                c = c.push(muted(p.exe.clone()).size(11).font(Font::MONOSPACE));
            }
            for conn in conns.iter().take(25) {
                let host = conn.host.clone().unwrap_or_else(|| "resolving…".into());
                let mut r = row![
                    text(host).size(12).width(Length::FillPortion(5)),
                    text(format!("{}:{}", conn.remote, conn.remote_port)).size(12).font(Font::MONOSPACE).width(Length::FillPortion(4)),
                    muted(format!("{} {}", conn.proto, conn.state)).size(11).width(Length::FillPortion(2)),
                ]
                .spacing(8)
                .align_y(Alignment::Center);
                r = r.push(match &conn.flag {
                    Some((label, level)) => container(pill(label.clone(), level_color(*level))).width(Length::FillPortion(3)),
                    None => container(Space::with_width(0)).width(Length::FillPortion(3)),
                });
                c = c.push(r);
            }
            if conns.len() > 25 {
                c = c.push(muted(format!("… and {} more", conns.len() - 25)).size(11));
            }
            let accent = if conns.iter().any(|c| c.flag.as_ref().is_some_and(|f| f.1 >= Level::Medium)) { Some(ORANGE) } else { None };
            body = body.push(card(c.into(), accent));
        }
        if shown == 0 {
            body = body.push(muted(if self.net_flagged { "No connections to known tracking servers right now." } else { "No outgoing connections right now." }.into()).size(13));
        }
        body = body.push(section_header("LISTENING FOR INCOMING CONNECTIONS", "Programs other devices could connect to. \"Network\" means reachable from your local network; \"this PC only\" is safe."));
        for l in &snap.listeners {
            let what = match l.port {
                22 => " — SSH remote login",
                53 => " — DNS",
                111 => " — rpcbind (NFS shares)",
                139 | 445 => " — Windows file sharing (Samba)",
                631 => " — printing (CUPS)",
                902 => " — VMware",
                3389 => " — Remote desktop",
                5353 => " — network discovery (mDNS)",
                5432 => " — PostgreSQL",
                3306 => " — MySQL",
                _ => "",
            };
            body = body.push(
                row![
                    text(format!("{} {}", l.proto, l.port)).size(12).font(Font::MONOSPACE).width(Length::Fixed(110.0)),
                    text(format!("{}{what}", l.process)).size(12).width(Length::Fill),
                    muted(l.addr.to_string()).size(11).width(Length::Fixed(200.0)),
                    if l.public { pill("network".into(), YELLOW) } else { pill("this PC only".into(), GREEN) },
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        page(self, "Network", "Who is talking to the internet right now", Some(refresh), body)
    }

    // ------------------------------------------------------------- startup

    pub(crate) fn startup_view(&self) -> Element<'_, Message> {
        let Some(items) = &self.startup_items else {
            return page(self, "Startup", "What starts automatically", None, loading("Reading services and login apps…"));
        };
        let busy = self.busy.is_some();
        let suggestions = items.iter().filter(|i| i.enabled && i.hint == Hint::Disable).count();
        let mut body = Column::new().push(
            row![
                stat_card("Running at startup", items.iter().filter(|i| i.enabled).count().to_string(), BLUE),
                stat_card("Suggested to turn off", suggestions.to_string(), if suggestions > 0 { ORANGE } else { GREEN }),
                stat_card("Turned off", items.iter().filter(|i| !i.enabled).count().to_string(), SLATE),
            ]
            .spacing(12),
        );
        for (title, sub, kinds) in [
            ("LOGIN APPS", "Programs that open when you log in.", vec![SKind::Autostart]),
            ("SERVICES", "Background services for your user and for the whole system.", vec![SKind::User, SKind::System]),
        ] {
            body = body.push(section_header(title, sub));
            for i in items.iter().filter(|i| kinds.contains(&i.kind)) {
                if self.startup_hints_only && !matches!(i.hint, Hint::Disable | Hint::Optional) {
                    continue;
                }
                let action = crate::startup::toggle_action(i, !i.enabled);
                let confirm = self.confirm_box(&action);
                let help = format!(
                    "{} {}. Runs: {}",
                    if i.enabled { "Turn off" } else { "Turn on" },
                    i.name,
                    advice::describe(&action.kind)
                );
                let mut head = row![
                    toggler(i.enabled)
                        .size(18)
                        .on_toggle_maybe((!busy && confirm.is_none()).then(|| {
                            let a = action.clone();
                            move |_| Message::RunAction(a.clone())
                        }))
                        .tip(help),
                    text(&i.name).size(14).font(BOLD),
                    pill(i.kind.label().into(), SLATE),
                ]
                .spacing(8)
                .align_y(Alignment::Center);
                if i.kind != SKind::Autostart && i.enabled != i.active {
                    head = head.push(pill(if i.active { "running" } else { "not running" }.into(), SLATE));
                }
                if i.hint != Hint::Unknown {
                    let color = match i.hint {
                        Hint::Disable => ORANGE,
                        Hint::Optional => BLUE,
                        _ => GREEN,
                    };
                    head = head.push(pill(i.hint.label().into(), color));
                }
                let mut c = Column::new().spacing(2).push(head);
                let desc = if i.why.is_empty() { i.description.clone() } else { i.why.clone() };
                if !desc.is_empty() {
                    c = c.push(muted(desc).size(12));
                }
                if let Some(b) = confirm {
                    c = c.push(b);
                }
                body = body.push(container(c).padding([4, 0]));
            }
        }
        page(self, "Startup", "Services and apps that start by themselves", None, body)
    }

    // ------------------------------------------------------------- sources

    pub(crate) fn sources_view(&self) -> Element<'_, Message> {
        let refresh = crate::sources::refresh_action(&self.sources).map(|a| -> Element<Message> {
            let help = format!("Download the latest package lists from all enabled repositories. Runs: {}", advice::describe(&a.kind));
            button(text(a.label.clone()).size(14))
                .padding([10, 16])
                .style(button::secondary)
                .on_press_maybe(self.busy.is_none().then(|| Message::RunAction(a)))
                .tip(help)
        });
        let Some(r) = &self.sources_report else {
            return page(self, "Sources", "Where your software comes from", refresh, loading("Reading repositories…"));
        };
        let subtitle: &str = if r.codename.is_empty() { "Where your software comes from" } else { "Where your software comes from, checked against your release" };
        let third: Vec<_> = r.repos.iter().filter(|x| !x.official).collect();
        let issues = third.iter().filter(|x| x.enabled && !x.issues.is_empty()).count();
        let mut body = Column::new().push(
            row![
                stat_card("Third-party sources", third.iter().filter(|x| x.enabled).count().to_string(), BLUE),
                stat_card("Need attention", issues.to_string(), if issues > 0 { ORANGE } else { GREEN }),
                stat_card("Packages without updates", r.orphans.len().to_string(), if r.orphans.is_empty() { GREEN } else { YELLOW }),
            ]
            .spacing(12),
        );
        body = body.push(section_header("REPOSITORIES", "Third-party sources first. Problems are listed on each card."));
        for repo in &r.repos {
            let mut head = row![text(&repo.name).size(14).font(BOLD), pill(repo.source.short().into(), SLATE)]
                .spacing(8)
                .align_y(Alignment::Center);
            head = head.push(if repo.official { pill("official".into(), GREEN) } else { pill("third-party".into(), BLUE) });
            if !repo.enabled {
                head = head.push(pill("disabled".into(), SLATE));
            }
            head = head.push(horizontal_space());
            let mut confirm = None;
            if let Some((disable, enable)) = &repo.toggle {
                let a = if repo.enabled { disable } else { enable };
                confirm = self.confirm_box(a);
                head = head.push(
                    button(text(a.label.clone()).size(12))
                        .style(if repo.enabled { button::secondary } else { button::primary })
                        .on_press_maybe((self.busy.is_none() && confirm.is_none()).then(|| Message::RunAction(a.clone()))).tip(if repo.enabled { "Stop using this repository (packages from it stay installed)" } else { "Use this repository again" }),
                );
            }
            let mut c = Column::new().spacing(4).push(head);
            c = c.push(muted(format!("{} {}", repo.url, repo.suites)).size(11).font(Font::MONOSPACE));
            if !repo.file.is_empty() {
                c = c.push(muted(repo.file.clone()).size(11));
            }
            for i in &repo.issues {
                c = c.push(text(format!("⚠ {i}")).size(12).color(ORANGE));
            }
            if !repo.installed.is_empty() {
                c = c.push(muted(format!("Installed from it ({}): {}", repo.installed.len(), list_preview(repo.installed.iter(), 15))).size(12));
            }
            if let Some(b) = confirm {
                c = c.push(b);
            }
            let accent = if repo.enabled && !repo.issues.is_empty() { Some(ORANGE) } else { None };
            if repo.official && repo.issues.is_empty() {
                body = body.push(container(c).padding([4, 12]));
            } else {
                body = body.push(card(c.into(), accent));
            }
        }
        body = body.push(section_header(
            "INSTALLED BUT NOT FROM ANY REPOSITORY",
            "Installed from a downloaded file, or their repository was removed. They get no updates, including security fixes. Remove the ones you don't need, or add their vendor's repository.",
        ));
        if r.orphans.is_empty() {
            body = body.push(muted("None.".into()).size(13));
        } else {
            body = body.push(muted(list_preview(r.orphans.iter(), 200)).size(12).font(Font::MONOSPACE));
        }
        page(self, "Sources", subtitle, refresh, body)
    }

    // ------------------------------------------------------------- history

    pub(crate) fn history_view(&self) -> Element<'_, Message> {
        let mut body = Column::new();
        if let Some(plan) = &self.import_plan {
            body = body.push(self.import_card(plan));
        }
        let Some(entries) = &self.history else {
            return page(self, "History", "What was installed, updated and removed", None, body.push(loading("Reading logs…")));
        };
        let q = self.history_query.to_lowercase();
        let matching: Vec<(usize, &crate::history::Entry)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                q.is_empty()
                    || e.command.to_lowercase().contains(&q)
                    || e.changes.iter().any(|c| c.package.to_lowercase().contains(&q))
            })
            .collect();
        body = body.push(muted(format!("{} entries", matching.len())).size(12));
        for (i, e) in matching.iter().take(self.history_limit) {
            let open = self.history_open.contains(i);
            let mut counts: Vec<(Op, usize)> = Vec::new();
            for c in &e.changes {
                match counts.iter_mut().find(|(o, _)| *o == c.op) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((c.op, 1)),
                }
            }
            let summary = if counts.is_empty() {
                e.summary.clone()
            } else {
                counts.iter().map(|(o, n)| format!("{n} {}", o.label())).collect::<Vec<_>>().join(", ")
            };
            let preview: String = if open || e.changes.is_empty() {
                String::new()
            } else {
                list_preview(e.changes.iter().filter(|c| !c.automatic).map(|c| &c.package).collect::<Vec<_>>().into_iter(), 6)
            };
            let mut head = row![
                muted(e.time.clone()).size(12).font(Font::MONOSPACE).width(Length::Fixed(130.0)),
                pill(e.source.short().into(), SLATE),
                text(e.command.chars().take(70).collect::<String>()).size(13).font(BOLD),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            if !e.by.is_empty() {
                head = head.push(muted(format!("by {}", e.by)).size(11));
            }
            head = head.push(horizontal_space()).push(muted(summary).size(12));
            if !e.changes.is_empty() {
                head = head.push(button(text(if open { "Hide" } else { "Show" }).size(12)).style(button::text).on_press(Message::ToggleEntry(*i)).tip(if open { "Hide the package list" } else { "Show every package changed in this step" }));
            }
            let mut c = Column::new().spacing(4).push(head);
            if !preview.is_empty() {
                c = c.push(muted(preview).size(11));
            }
            if open {
                for ch in &e.changes {
                    let color = match ch.op {
                        Op::Install | Op::Reinstall => GREEN,
                        Op::Remove | Op::Purge => RED,
                        Op::Upgrade => BLUE,
                        _ => SLATE,
                    };
                    let mut r = row![pill(ch.op.label().into(), color), text(ch.package.clone()).size(12).font(Font::MONOSPACE)]
                        .spacing(8)
                        .align_y(Alignment::Center);
                    if !ch.version.is_empty() {
                        r = r.push(muted(short_version(&ch.version)).size(11));
                    }
                    if ch.automatic {
                        r = r.push(muted("(dependency)".into()).size(11));
                    }
                    c = c.push(r);
                }
                let mut actions = row![].spacing(8);
                let mut confirms = Vec::new();
                for a in crate::history::entry_actions(e, &self.packages) {
                    if let Some(b) = self.confirm_box(&a) {
                        confirms.push(b);
                    }
                    actions = actions.push(
                        button(text(a.label.clone()).size(12))
                            .style(button::secondary)
                            .on_press_maybe(self.busy.is_none().then(|| Message::RunAction(a.clone()))).tip(advice::describe(&a.kind)),
                    );
                }
                c = c.push(actions);
                for b in confirms {
                    c = c.push(b);
                }
            }
            body = body.push(card(c.into(), None));
        }
        if matching.len() > self.history_limit {
            body = body.push(
                container(button(text(format!("Show more ({} remaining)", matching.len() - self.history_limit)).size(13)).style(button::secondary).on_press(Message::HistoryMore).tip("Load 100 more entries"))
                    .center_x(Length::Fill),
            );
        }
        page(self, "History", "What was installed, updated and removed, and by whom", None, body)
    }

    fn import_card<'a>(&'a self, plan: &'a crate::history::Plan) -> Element<'a, Message> {
        let mut c = Column::new().spacing(6).push(
            row![
                text(format!("Import from {}", plan.file.rsplit('/').next().unwrap_or(""))).size(15).font(BOLD),
                horizontal_space(),
                button(text("Close").size(12)).style(button::text).on_press(Message::ClearImport).tip("Close this import summary"),
            ]
            .align_y(Alignment::Center),
        );
        c = c.push(text(format!("{} entries: {} already installed.", plan.total, plan.already)).size(13));
        for (src, list) in &plan.missing {
            c = c.push(muted(format!("To install from {} ({}): {}", src.label(), list.len(), list_preview(list.iter(), 25))).size(12));
        }
        if !plan.unavailable.is_empty() {
            c = c.push(muted(format!("Can't install ({}): {}", plan.unavailable.len(), list_preview(plan.unavailable.iter(), 15))).size(12));
        }
        match &plan.action {
            Some(a) => {
                let confirm = self.confirm_box(a);
                c = c.push(
                    button(text(a.label.clone()).size(13))
                        .style(button::primary)
                        .on_press_maybe((self.busy.is_none() && confirm.is_none()).then(|| Message::RunAction(a.clone()))).tip(advice::describe(&a.kind)),
                );
                if let Some(b) = confirm {
                    c = c.push(b);
                }
            }
            None => c = c.push(text("Everything from this list is already installed.").size(13).color(GREEN)),
        }
        card(c.into(), Some(BLUE))
    }

    // ------------------------------------------------------- AppImage install

    pub(crate) fn appimage_picker(&self) -> Element<'_, Message> {
        let c = column![
            text("Install an AppImage").size(16).font(BOLD),
            muted("Downloaded a .AppImage file? Choose it here and it will be kept in ~/Applications, get an icon in your app menu, and show up in the Installed tab, where you can uninstall it like any other app.".into()).size(12),
            row![
                text_input("Path to an .AppImage file…", &self.appimage_path)
                    .on_input(Message::AppImagePath)
                    .on_submit(Message::InspectAppImage)
                    .padding(8)
                    .size(13),
                button(text("Browse…").size(13))
                    .style(button::secondary)
                    .on_press_maybe((!self.appimage_busy).then_some(Message::BrowseAppImage))
                    .tip("Choose the file in a file dialog"),
                button(text("Open").size(13))
                    .style(button::primary)
                    .on_press_maybe((!self.appimage_busy && !self.appimage_path.trim().is_empty()).then_some(Message::InspectAppImage))
                    .tip("Read the app's name, icon and version from this file"),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(10);
        card(c.into(), None)
    }

    pub(crate) fn appimage_panel(&self) -> Element<'_, Message> {
        let Some(i) = &self.appimage else {
            return container(crate::loading_box("Reading the AppImage…")).center(Length::Fill).into();
        };
        let icon: Element<Message> = match &i.icon {
            Some(path) if std::fs::read(path).map(|b| b.starts_with(b"\x89PNG")).unwrap_or(false) => {
                iced::widget::image(iced::widget::image::Handle::from_path(path)).width(64).height(64).into()
            }
            Some(path) => iced::widget::svg(iced::widget::svg::Handle::from_path(path)).width(64).height(64).into(),
            None => container(text("⬢").size(40)).width(64).height(64).center(64).into(),
        };
        let mut pills = row![pill("AppImage".into(), SLATE)].spacing(6);
        if !i.version.is_empty() {
            pills = pills.push(pill(i.version.clone(), SLATE));
        }
        pills = pills.push(pill(crate::human_size(i.size), SLATE));
        let mut col = Column::new().spacing(14).padding(22).push(
            row![icon, column![text(&i.name).size(24).font(BOLD), pills].spacing(6)].spacing(14).align_y(Alignment::Center),
        );
        if !i.comment.is_empty() {
            col = col.push(text(&i.comment).size(14));
        }
        if !i.categories.is_empty() {
            col = col.push(muted(format!("Categories: {}", i.categories.trim_end_matches(';').replace(';', ", "))).size(12));
        }
        let file = std::path::Path::new(&i.staged).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let action = Action {
            label: "Install".into(),
            kind: ActionKind::InstallAppImage(Box::new(i.clone())),
            confirm: None,
        };
        let help = advice::describe(&action.kind);
        col = col.push(
            row![
                button(text("Install").size(14))
                    .padding([9, 22])
                    .style(button::primary)
                    .on_press_maybe(self.busy.is_none().then(|| Message::RunAction(action)))
                    .tip(help),
                button(text("Cancel").size(14)).padding([9, 16]).style(button::secondary).on_press(Message::ClearAppImage).tip("Don't install it"),
            ]
            .spacing(10),
        );
        col = col.push(muted(format!("It will be saved as ~/Applications/{file} and added to your app menu. Your original file stays where it is.")).size(12));
        if let Some(w) = &i.warning {
            col = col.push(card(text(w.clone()).size(12).into(), Some(YELLOW)));
        }
        if i.needs_fuse2 {
            col = col.push(card(
                text("FUSE 2 isn't installed, so the app will unpack itself each time it starts (slower). Installing the libfuse2 package fixes this.").size(12).into(),
                Some(YELLOW),
            ));
        }
        col = col.push(card(
            text("AppImages aren't sandboxed or checked by your distribution, and they don't update automatically. Only install apps from developers you trust.").size(12).into(),
            Some(ORANGE),
        ));
        scrollable(col).height(Length::Fill).into()
    }

    // ------------------------------------------------------- details popup

    pub(crate) fn popup_view<'a>(&'a self, key: &'a crate::Key) -> Element<'a, Message> {
        use crate::{human_size, kv, list_section, privacy_section, verdict_card, BOLD};
        let close = button(text("Close").size(13)).style(button::secondary).on_press(Message::ClosePopup).tip("Close (Esc)");
        let Some(p) = self.packages.iter().find(|p| p.source == key.0 && p.id == key.1) else {
            return popup_frame(column![text("This package is no longer installed.").size(14), close].spacing(12).into());
        };
        let mut pills = row![pill(p.source.label().into(), SLATE), pill(format!("{:?}", p.kind).to_lowercase(), SLATE), pill(short_version(&p.version), SLATE)].spacing(6);
        if let Some(s) = p.size_bytes {
            pills = pills.push(pill(human_size(s), SLATE));
        }
        if let Some(l) = p.telemetry {
            pills = pills.push(pill(format!("privacy: {}", l.label()), level_color(l)));
        }
        let mut col = Column::new().spacing(14).push(
            row![
                column![text(&p.name).size(24).font(BOLD), muted(p.id.clone()).size(12), pills].spacing(6).width(Length::Fill),
                close,
            ]
            .align_y(Alignment::Start),
        );
        if !p.summary.is_empty() {
            col = col.push(text(&p.summary).size(14));
        }
        let Some(d) = &self.popup_details else {
            return popup_frame(col.push(crate::loading_box("Analyzing…")).into());
        };
        let twins = self
            .packages
            .iter()
            .filter(|o| o.source != p.source && (o.name.eq_ignore_ascii_case(&p.name) || o.id == p.id))
            .count();
        let verdict = advice::verdict(p, d, twins);

        // Actions
        let busy = self.busy.is_some();
        let mut actions = row![].spacing(10).align_y(Alignment::Center);
        let mut confirm = None;
        if verdict.risk != advice::Risk::Danger {
            let mut q = format!("Uninstall {} ({})?", p.name, p.source.label());
            if !verdict.reasons.is_empty() {
                q.push(' ');
                q.push_str(&verdict.reasons.join(" "));
            }
            let action = Action { label: "Uninstall".into(), kind: ActionKind::Remove(vec![p.clone()]), confirm: Some(q) };
            confirm = self.confirm_box(&action);
            actions = actions.push(
                button(text("Uninstall").size(14))
                    .padding([9, 16])
                    .style(button::danger)
                    .on_press_maybe((!busy && confirm.is_none()).then(|| Message::RunAction(action)))
                    .tip("Remove this package (you confirm first)"),
            );
        }
        actions = actions.push(
            button(text("Open in Installed").size(14))
                .padding([9, 16])
                .style(button::secondary)
                .on_press(Message::OpenPackage((p.source, p.id.clone())))
                .tip("Show this package in the Installed tab, with update and sandbox controls"),
        );
        col = col.push(actions);
        if let Some(b) = confirm {
            col = col.push(b);
        }
        if let Some(b) = &self.busy {
            col = col.push(card(text(b.clone()).size(13).into(), Some(BLUE)));
        }
        col = col.push(verdict_card(&verdict));
        if let Some(a) = crate::about_section(d) {
            col = col.push(a);
        }

        let running: Vec<String> = d.running.iter().map(|(pid, c)| format!("[{pid}] {c}")).collect();
        col = col
            .push(list_section(format!("USED BY ({})", d.used_by.len()), &d.used_by, "No other packages depend on it."))
            .push(list_section(format!("RUNNING NOW ({})", running.len()), &running, "Not running."))
            .push(privacy_section(&d.telemetry))
            .push(list_section(format!("DEPENDS ON ({})", d.depends_on.len()), &d.depends_on, "No dependencies."))
            .push(list_section("LOCATION".into(), &d.location, "Unknown."));
        if !d.info.is_empty() {
            let mut c = Column::new().spacing(4).push(section_title("DETAILS"));
            for (k, v) in &d.info {
                c = c.push(kv(k, v));
            }
            col = col.push(c);
        }
        popup_frame(col.into())
    }

    // ------------------------------------------------------- flatpak switches

    pub(crate) fn switches_section<'a>(&'a self, p: &'a crate::model::Package, switches: &'a [Switch]) -> Element<'a, Message> {
        let busy = self.busy.is_some();
        let mut c = Column::new().spacing(6).push(section_title("SANDBOX PERMISSIONS"));
        for sw in switches {
            let flag = if sw.on { sw.disable.clone() } else { sw.enable.clone() };
            let action = Action {
                label: format!("{} {}", if sw.on { "Blocking" } else { "Allowing" }, sw.label.to_lowercase()),
                kind: ActionKind::User(vec!["flatpak".into(), "override".into(), "--user".into(), flag, p.id.clone()]),
                confirm: None,
            };
            c = c.push(
                row![
                    toggler(sw.on)
                        .size(18)
                        .on_toggle_maybe((!busy).then(|| move |_| Message::RunAction(action.clone())))
                        .tip(if sw.on { "Click to block this for the app" } else { "Click to allow this for the app" }),
                    column![text(sw.label).size(13), muted(sw.help.to_string()).size(11)].spacing(1),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }
        c = c.push(
            row![
                button(text("Reset to app defaults").size(12)).style(button::secondary).on_press_maybe((!busy).then(|| {
                    Message::RunAction(Action {
                        label: "Resetting permissions".into(),
                        kind: ActionKind::User(["flatpak", "override", "--user", "--reset", &p.id].map(String::from).to_vec()),
                        confirm: None,
                    })
                })).tip("Undo all your permission changes for this app"),
                muted("Changes apply the next time the app starts.".into()).size(11),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
        let _ = advice::Risk::Safe;
        c.into()
    }
}

fn popup_frame(content: Element<'_, Message>) -> Element<'_, Message> {
    container(scrollable(container(content).padding(iced::Padding { right: 12.0, ..Default::default() })))
        .padding(24)
        .width(Length::Fixed(620.0))
        .max_height(720.0)
        .style(|t: &iced::Theme| {
            let pal = t.extended_palette();
            iced::widget::container::Style {
                background: Some(pal.background.base.color.into()),
                border: iced::Border { radius: 14.0.into(), width: 1.0, color: pal.background.strong.color },
                shadow: iced::Shadow { color: iced::Color { a: 0.4, ..iced::Color::BLACK }, offset: iced::Vector::new(0.0, 8.0), blur_radius: 30.0 },
                ..Default::default()
            }
        })
        .into()
}

impl App {
    /// "A new version of Package Tracker is available" popup.
    /// Shown once, before first use: what the app can do, and that it's at your own risk.
    pub(crate) fn disclaimer_view(&self) -> Element<'_, Message> {
        let point = |t: &'static str| row![text("•").size(14), text(t).size(14)].spacing(8);
        let col = Column::new()
            .spacing(14)
            .push(
                column![
                    text("Before you start").size(22).font(BOLD),
                    muted("Package Tracker can change your system. Please read this once.".into()).size(14),
                ]
                .spacing(6),
            )
            .push(text(
                "It can install, remove and update software, turn services off and run commands as administrator. \
                 Its verdicts and suggestions come from heuristics and your package manager's data, and can be wrong for your system.",
            ).size(14))
            .push(
                column![
                    point("Every change shows its exact command and asks before it runs. Read it before you confirm."),
                    point("Some changes can't be undone. Keep backups of anything important."),
                    point("If you're not sure about a change, don't confirm it."),
                ]
                .spacing(6),
            )
            .push(
                text(
                    "You use Package Tracker at your own risk. It is provided \u{201c}as is\u{201d}, without warranty (MIT license), \
                     and its authors are not responsible for any damage or data loss, whether by mistake or on purpose.",
                )
                .size(14)
                .font(BOLD),
            )
            .push(
                row![
                    button(text("Read the full disclaimer").size(14))
                        .padding([9, 16])
                        .style(button::secondary)
                        .on_press(Message::OpenUrl("https://github.com/pkg-tracker/pkgtrack#disclaimer".into()))
                        .tip("Open the disclaimer on GitHub"),
                    horizontal_space(),
                    button(text("I understand").size(14))
                        .padding([9, 16])
                        .style(button::primary)
                        .on_press(Message::AcceptDisclaimer)
                        .tip("Don't show this again"),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        popup_frame(col.into())
    }

    pub(crate) fn app_update_view<'a>(&'a self, rel: &'a crate::update::Release) -> Element<'a, Message> {
        let mut col = Column::new().spacing(14).push(
            column![
                text("A new version is available").size(22).font(BOLD),
                muted(format!("Package Tracker {} is out. You have {}.", rel.version, crate::update::CURRENT)).size(14),
            ]
            .spacing(6),
        );
        if !rel.notes.is_empty() {
            col = col.push(section_title("WHAT'S NEW")).push(text(&rel.notes).size(13));
        }
        let download = button(text("Download").size(14))
            .padding([9, 16])
            .style(button::success)
            .on_press_maybe((!rel.url.is_empty()).then(|| Message::OpenUrl(rel.url.clone())))
            .tip("Open the release page to download the new .deb or AppImage");
        let later = button(text("Later").size(14)).padding([9, 16]).style(button::secondary).on_press(Message::CloseAppUpdate).tip("Remind me next time (Esc)");
        let skip = button(text("Skip this version").size(14))
            .padding([9, 16])
            .style(button::text)
            .on_press(Message::SkipAppUpdate)
            .tip("Don't remind me about this version again");
        col = col.push(row![download, later, horizontal_space(), skip].spacing(10).align_y(Alignment::Center));
        popup_frame(col.into())
    }
}
