const DBUS_INTERFACE: &str = "com.makuluni.swayscratchpad";
const DBUS_PATH: &str = "/scratchpads";
const HIDE: &str = "move to scratchpad";

async fn usage<T: std::io::Write>(cmd: &str, conn: Conn, mut t: T) {
    let cmd = cmd.split('/').last().unwrap();
    writeln!(
        t,
        "Usage:\n  Toggle a scratchpad app: \x1b[35m{cmd} <app>\x1b[0m"
    )
    .unwrap();
    if let Ok(list) = conn.list().await {
        if !list.is_empty() {
            writeln!(t, "    Currently configured apps are:").unwrap();
            for app in list {
                writeln!(t, "      * {app}").unwrap();
            }
        }
    }
    writeln!(t, "  Launch the scratchpad daemon: \x1b[35m{cmd} -d\x1b[0m").unwrap();
    writeln!(
        t,
        "  Configure a new scratchpad app: \x1b[35m{cmd} -a <name> <id> <cmd> <args..>\x1b[0m"
    )
    .unwrap();
    writeln!(
        t,
        "  Remove an app from the configuration: \x1b[35m{cmd} -r <name>\x1b[0m"
    )
    .unwrap();
    writeln!(t, "  Save current configurations: \x1b[35m{cmd} -s\x1b[0m").unwrap();
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Scratchpad<T: AsRef<str> = String> {
    name: T,
    id: T,
    exec: T,
    args: Vec<T>,
}
impl Scratchpad {
    fn launch(&self) {
        if let Ok(fork::Fork::Child) = fork::fork() {
            if fork::setsid().is_ok() {
                std::process::Command::new(&self.exec)
                    .args(&self.args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
            }
            std::process::exit(0);
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Status {
    active: std::collections::HashSet<i64>,
    showing: bool,
}

#[derive(Clone)]
struct ScratchpadStore(
    std::sync::Arc<(
        std::sync::RwLock<std::collections::HashMap<String, Scratchpad>>,
        std::sync::RwLock<std::collections::HashMap<String, Status>>,
    )>,
);
impl ScratchpadStore {
    async fn new() -> Self {
        let pads: std::collections::HashMap<String, Scratchpad> = dirs::config_dir()
            .map(|mut d| {
                d.push("sway-scratchpads");
                d.push("config.toml");
                d
            })
            .and_then(|d| match std::fs::read_to_string(d) {
                Ok(o) => match toml::from_str(&o) {
                    Ok(o) => Some(o),
                    Err(e) => {
                        tracing::error!(kind = "config", "{e}");
                        None
                    }
                },
                Err(e) => {
                    tracing::error!(kind = "config", "{e}");
                    None
                }
            })
            .unwrap_or_default();
        let mut status: std::collections::HashMap<String, Status> = pads
            .iter()
            .map(|(_, s)| {
                (
                    s.id.clone(),
                    Status {
                        showing: true,
                        active: Default::default(),
                    },
                )
            })
            .collect();
        if !pads.is_empty() {
            match swayipc::Connection::new().and_then(|mut sway| sway.get_tree()) {
                Ok(c) => {
                    for t in c.iter() {
                        if let Some(aid) = t.app_id.as_ref() {
                            if let Some(v) = status.get_mut(aid) {
                                v.active.insert(t.id);
                            }
                        }
                        for t in &t.floating_nodes {
                            if let Some(aid) = t.app_id.as_ref() {
                                if let Some(v) = status.get_mut(aid) {
                                    v.active.insert(t.id);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(kind = "swayipc", "{e}");
                    std::process::exit(10);
                }
            };
        }
        let me = Self(std::sync::Arc::new((
            std::sync::RwLock::new(pads),
            std::sync::RwLock::new(status),
        )));
        let r = me.clone();
        tokio::spawn(async move {
            'outer: loop {
                let mut events = match swayipc::Connection::new()
                    .and_then(|sway| sway.subscribe(&[swayipc::EventType::Window]))
                {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::error!(kind = "swayipc", "{e}");
                        std::process::exit(10);
                    }
                };
                while let Some(event) = events.next() {
                    let event = match event {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::error!(kind = "swayipc", "{e}");
                            continue 'outer;
                        }
                    };
                    if let swayipc::Event::Window(event) = event {
                        match event.change {
                            swayipc::WindowChange::New => {
                                if let Some(mut w) = me.status_write() {
                                    if let Some(status) =
                                        event.container.app_id.as_ref().and_then(|i| w.get_mut(i))
                                    {
                                        match swayipc::Connection::new() {
                                            Err(e) => {
                                                tracing::error!(kind = "swayipc", "{e}");
                                            }
                                            Ok(mut conn) => {
                                                if status.showing {
                                                    show(&mut conn, event.container.id);
                                                } else {
                                                    let _ = conn.run_command(format!(
                                                        "[con_id={}] {HIDE}",
                                                        event.container.id
                                                    ));
                                                }
                                                status.active.insert(event.container.id);
                                            }
                                        }
                                    }
                                }
                            }
                            swayipc::WindowChange::Close => {
                                if let Some(mut w) = me.status_write() {
                                    if let Some(status) =
                                        event.container.app_id.as_ref().and_then(|i| w.get_mut(i))
                                    {
                                        status.active.remove(&event.container.id);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        });
        r
    }
    fn contains_key(&self, k: &str) -> bool {
        if let Some(read) = self.pads_read() {
            read.contains_key(k)
        } else {
            false
        }
    }
    fn keys(&self) -> Vec<String> {
        if let Some(read) = self.pads_read() {
            read.keys().cloned().collect()
        } else {
            vec![]
        }
    }
    fn add(&self, s: Scratchpad) -> CmdResult {
        let mut conn = match swayipc::Connection::new() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(kind = "swayipc", "{e}");
                return CmdResult::Err(e.to_string());
            }
        };
        let active = match conn.get_tree() {
            Ok(c) => c.iter().fold(std::collections::HashSet::new(), |mut v, t| {
                if t.app_id.as_ref().is_some_and(|i| i == &s.id) {
                    v.insert(t.id);
                }
                for t in &t.floating_nodes {
                    if t.app_id.as_ref().is_some_and(|i| i == &s.id) {
                        v.insert(t.id);
                    }
                }

                v
            }),
            Err(e) => {
                tracing::error!(kind = "swayipc", "{e}");
                return CmdResult::Err(e.to_string());
            }
        };
        if !active.is_empty() {
            for &id in &active {
                show(&mut conn, id);
            }
        }
        if let (Some(mut pads), Some(mut status)) = (self.pads_write(), self.status_write()) {
            status.insert(
                s.id.clone(),
                Status {
                    active,
                    showing: true,
                },
            );
            pads.insert(s.name.clone(), s);
            CmdResult::Ok
        } else {
            return CmdResult::local_data_err();
        }
    }
    fn toggle(&self, name: &str) -> CmdResult {
        let Some(r) = self.pads_read() else {
            return CmdResult::local_data_err();
        };
        if let Some(s) = r.get(name) {
            let Some(mut w) = self.status_write() else {
                return CmdResult::local_data_err();
            };
            if let Some(a) = w
                .get_mut(&s.id)
                .and_then(|a| (!a.active.is_empty()).then_some(a))
            {
                let mut conn = match swayipc::Connection::new() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(kind = "swayipc", "{e}");
                        return CmdResult::Err(e.to_string());
                    }
                };
                if a.showing {
                    for c in &a.active {
                        let _ = conn.run_command(format!("[con_id={c}] {HIDE}"));
                    }
                    a.showing = false;
                } else {
                    for c in &a.active {
                        show(&mut conn, *c);
                    }
                    a.showing = true;
                }
            } else {
                s.launch();
            }
            CmdResult::Ok
        } else {
            CmdResult::BadInput
        }
    }
    fn remove(&self, name: &str) -> CmdResult {
        let Some(mut w) = self.pads_write() else {
            return CmdResult::local_data_err();
        };
        if let Some(a) = w.remove(name) {
            let Some(mut w) = self.status_write() else {
                return CmdResult::local_data_err();
            };
            w.remove(&a.id);
            CmdResult::Ok
        } else {
            CmdResult::BadInput
        }
    }
    fn save(&self) -> CmdResult {
        let data = {
            let Some(r) = self.pads_read() else {
                return CmdResult::local_data_err();
            };
            match toml::to_string(&*r) {
                Ok(d) => d,
                Err(e) => return CmdResult::Err(e.to_string()),
            }
        };
        let Some(mut config) = dirs::config_dir() else {
            tracing::error!(kind = "config", "Could not determine config directory.");
            return CmdResult::Err("Could not determine config directory.".to_string());
        };
        if !config.exists() {
            tracing::error!(kind = "config", "Could not determine config directory.");
            return CmdResult::Err("Could not determine config directory.".to_string());
        }
        config.push("sway-scratchpads");
        if !config.exists() {
            if let Err(e) = std::fs::create_dir(&config) {
                tracing::error!(kind = "config", "Could not create config directory: {e}");
                return CmdResult::Err(format!("Could not create config directory: {e}"));
            }
        }
        config.push("config.toml");
        match std::fs::write(&config, data) {
            Ok(()) => CmdResult::Ok,
            Err(e) => {
                tracing::error!(kind = "config", "Could not write config file: {e}");
                return CmdResult::Err(format!("Could not write config file: {e}"));
            }
        }
    }
    fn pads_read(
        &self,
    ) -> Option<std::sync::RwLockReadGuard<std::collections::HashMap<String, Scratchpad>>> {
        match self.0 .0.read() {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::error!("Failed to get write guard: {e}");
                None
            }
        }
    }
    #[allow(unused)]
    fn status_read(
        &self,
    ) -> Option<std::sync::RwLockReadGuard<std::collections::HashMap<String, Status>>> {
        match self.0 .1.read() {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::error!("Failed to get write guard: {e}");
                None
            }
        }
    }
    fn pads_write(
        &self,
    ) -> Option<std::sync::RwLockWriteGuard<std::collections::HashMap<String, Scratchpad>>> {
        match self.0 .0.write() {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::error!("Failed to get write guard: {e}");
                None
            }
        }
    }
    fn status_write(
        &self,
    ) -> Option<std::sync::RwLockWriteGuard<std::collections::HashMap<String, Status>>> {
        match self.0 .1.write() {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::error!("Failed to get write guard: {e}");
                None
            }
        }
    }
}

async fn daemon() {
    let mut cr = dbus_crossroads::Crossroads::new();

    let (resource, c) = dbus_tokio::connection::new_session_sync().unwrap();
    let _handle = tokio::spawn(async {
        let err = resource.await;
        panic!("Lost connection to DBus: {}", err);
    });

    cr.set_async_support(Some((
        c.clone(),
        Box::new(|x| {
            tokio::spawn(x);
        }),
    )));

    let iface_token = cr.register(DBUS_INTERFACE, |b| {
        b.method_with_cr(
            "Add",
            ("name", "id", "exec", "args"),
            ("success",),
            |ctx, cr, (name, id, exec, args): (String, String, String, Vec<String>)| {
                let data = cr
                    .data_mut::<ScratchpadStore>(ctx.path())
                    .expect("Could not get shared data.")
                    .clone();
                let ok = if data.contains_key(&name) {
                    "bad_input".to_string()
                } else {
                    let s = Scratchpad {
                        name,
                        id,
                        exec,
                        args,
                    };
                    data.add(s).to_string()
                };
                Ok((ok,))
            },
        );
        b.method_with_cr("List", (), ("scratchpads",), |ctx, cr, (): ()| {
            let data = cr
                .data_mut::<ScratchpadStore>(ctx.path())
                .expect("Could not get shared data.")
                .clone();
            let keys = data.keys();
            Ok((keys,))
        });
        b.method_with_cr(
            "Toggle",
            ("name",),
            ("success",),
            |ctx, cr, (name,): (String,)| {
                let data = cr
                    .data_mut::<ScratchpadStore>(ctx.path())
                    .expect("Could not get shared data.")
                    .clone();
                Ok((data.toggle(&name).to_string(),))
            },
        );
        b.method_with_cr(
            "Remove",
            ("name",),
            ("success",),
            |ctx, cr, (name,): (String,)| {
                let data = cr
                    .data_mut::<ScratchpadStore>(ctx.path())
                    .expect("Could not get shared data.")
                    .clone();
                Ok((data.remove(&name).to_string(),))
            },
        );
        b.method_with_cr("Save", (), ("success",), |ctx, cr, (): ()| {
            let data = cr
                .data_mut::<ScratchpadStore>(ctx.path())
                .expect("Could not get shared data.")
                .clone();
            Ok((data.save().to_string(),))
        });
    });
    cr.insert(DBUS_PATH, &[iface_token], ScratchpadStore::new().await);

    use dbus::channel::MatchingReceiver;
    c.start_receive(
        dbus::message::MatchRule::new_method_call(),
        Box::new(move |msg, conn| {
            cr.handle_message(msg, conn).unwrap();
            true
        }),
    );

    c.request_name(DBUS_INTERFACE, false, true, false)
        .await
        .unwrap();

    futures::future::pending::<()>().await;
    unreachable!()
}

struct Conn(dbus::nonblock::Proxy<'static, std::sync::Arc<dbus::nonblock::SyncConnection>>);
impl Conn {
    async fn new() -> Self {
        let (resource, conn) = dbus_tokio::connection::new_session_sync().unwrap();
        let _handle = tokio::spawn(async {
            let err = resource.await;
            panic!("Lost connection to D-Bus: {}", err);
        });

        Self(dbus::nonblock::Proxy::new(
            DBUS_INTERFACE,
            DBUS_PATH,
            std::time::Duration::from_secs(1),
            conn,
        ))
    }
    async fn add(&self, scratchpad: Scratchpad) -> Result<CmdResult, dbus::Error> {
        let (ok,): (String,) = self
            .0
            .method_call(
                DBUS_INTERFACE,
                "Add",
                (
                    scratchpad.name,
                    scratchpad.id,
                    scratchpad.exec,
                    scratchpad.args,
                ),
            )
            .await?;
        Ok(ok.into())
    }
    async fn list(&self) -> Result<Vec<String>, dbus::Error> {
        let (r,) = self.0.method_call(DBUS_INTERFACE, "List", ()).await?;

        Ok(r)
    }
    async fn toggle(&self, scratchpad: String) -> Result<CmdResult, dbus::Error> {
        let (r,): (String,) = self
            .0
            .method_call(DBUS_INTERFACE, "Toggle", (scratchpad,))
            .await?;

        Ok(r.into())
    }
    async fn remove(&self, scratchpad: String) -> Result<CmdResult, dbus::Error> {
        let (r,): (String,) = self
            .0
            .method_call(DBUS_INTERFACE, "Remove", (scratchpad,))
            .await?;

        Ok(r.into())
    }
    async fn save(&self) -> Result<CmdResult, dbus::Error> {
        let (r,): (String,) = self.0.method_call(DBUS_INTERFACE, "Save", ()).await?;

        Ok(r.into())
    }
}
enum CmdResult {
    Ok,
    Err(String),
    BadInput,
}
impl CmdResult {
    fn local_data_err() -> Self {
        CmdResult::Err("Could not access local data.".to_string())
    }
}
impl ToString for CmdResult {
    fn to_string(&self) -> String {
        match self {
            Self::Ok => "ok".to_string(),
            Self::Err(e) => e.clone(),
            Self::BadInput => "bad_input".to_string(),
        }
    }
}
impl From<String> for CmdResult {
    fn from(s: String) -> Self {
        match s.as_str() {
            "ok" => Self::Ok,
            "bad_input" => Self::BadInput,
            _ => Self::Err(s),
        }
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args();
    let cmd = args.next().unwrap();
    if let Some(arg) = args.next() {
        match arg.as_str() {
            "-d" => {
                if args.next().is_none() {
                    daemon().await;
                } else {
                    let conn = Conn::new().await;
                    usage(&cmd, conn, std::io::stderr()).await;
                    std::process::exit(1);
                }
            }
            "-a" => {
                let conn = Conn::new().await;
                let (Some(name), Some(id), Some(exec)) = (args.next(), args.next(), args.next())
                else {
                    usage(&cmd, conn, std::io::stderr()).await;
                    std::process::exit(1)
                };
                match conn
                    .add(Scratchpad {
                        name: name.clone(),
                        id,
                        exec,
                        args: args.collect(),
                    })
                    .await
                    .unwrap()
                {
                    CmdResult::Ok => {}
                    CmdResult::BadInput => {
                        eprintln!("Scratchpad {name:?} already exists.");
                        std::process::exit(2)
                    }
                    CmdResult::Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(10)
                    }
                }
            }
            "-r" => {
                let conn = Conn::new().await;
                let Some(name) = args.next() else {
                    usage(&cmd, conn, std::io::stderr()).await;
                    std::process::exit(1)
                };
                match conn.remove(name.clone()).await.unwrap() {
                    CmdResult::Ok => {}
                    CmdResult::BadInput => {
                        eprintln!("Scratchpad {name:?} was already unconfigured.");
                    }
                    CmdResult::Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(10)
                    }
                }
            }
            "-s" => {
                let conn = Conn::new().await;
                match conn.save().await.unwrap() {
                    CmdResult::Ok => {}
                    CmdResult::BadInput => unreachable!(),
                    CmdResult::Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(10)
                    }
                }
            }
            _ => {
                let conn = Conn::new().await;
                if arg.starts_with('-') || args.next().is_some() {
                    usage(&cmd, conn, std::io::stderr()).await;
                    std::process::exit(1)
                }
                match conn.toggle(arg.clone()).await.unwrap() {
                    CmdResult::Ok => {}
                    CmdResult::BadInput => {
                        eprintln!("Scratchpad {arg:?} is unconfigured.");
                        std::process::exit(2)
                    }
                    CmdResult::Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(2)
                    }
                }
            }
        }
    } else {
        let conn = Conn::new().await;
        usage(&cmd, conn, std::io::stderr()).await;
        std::process::exit(1);
    }
}

fn show(conn: &mut swayipc::Connection, win: i64) {
    let _ = conn.run_command(format!("[con_id={win}] scratchpad show;"));
    let _ = conn.run_command(format!(
        "[con_id={win}] move to workspace current; [con_id={win}] floating enable; [con_id={win}] resize set width 80ppt; [con_id={win}] resize set height 80ppt; [con_id={win}] move position center"
    ));
}
