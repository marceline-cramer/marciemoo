use std::{collections::HashMap, fmt::Display, sync::Arc};

use sled::Tree;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc::UnboundedSender},
};

pub struct State {
    tree: Tree,
    announcement_tx: broadcast::Sender<String>,
}

impl Default for State {
    fn default() -> Self {
        let db = sled::open("marciemoo.db").unwrap();
        let tree = db.open_tree("").unwrap();
        let announcement_tx = broadcast::Sender::new(1024);

        Self {
            tree,
            announcement_tx,
        }
    }
}

impl State {
    /// Creates a new object, and returns its new ID.
    pub fn create(&self) -> usize {
        self.tree
            .transaction::<_, _, ()>(|tx| {
                let id = tx.get("object-index")?.unwrap_or("0".into());
                let id = String::from_utf8(id.to_vec()).unwrap();
                let id: usize = id.parse().unwrap();

                let next = id + 1;
                let next = format!("{}", next);
                tx.insert("object-index", next.into_bytes())?;

                tx.insert(format!("object-exists-{id}").into_bytes(), "")?;

                Ok(id)
            })
            .unwrap()
    }

    /// Tests if an object exists by ID.
    pub fn exists(&self, id: usize) -> bool {
        self.tree
            .contains_key(format!("object-exists-{id}"))
            .unwrap()
    }

    /// Lists all of the objects.
    pub fn list(&self) -> Vec<usize> {
        let prefix = "object-exists-";
        let prefix_len = prefix.len();
        let exists = self.tree.scan_prefix(prefix);

        let mut ids = Vec::new();
        for exist in exists {
            let (key, _value) = exist.unwrap();
            let id = key[prefix_len..].to_vec();
            let id = String::from_utf8(id).unwrap();
            let id: usize = id.parse().unwrap();
            ids.push(id);
        }

        ids
    }

    /// Shows all the field names on an object.
    pub fn show(&self, id: usize) -> Vec<String> {
        let prefix = format!("object-field-{id}-");
        let prefix_len = prefix.len();
        let fields = self.tree.scan_prefix(prefix.into_bytes());

        let mut field_names = Vec::new();
        for field in fields {
            let (key, _value) = field.unwrap();
            let name = key[prefix_len..].to_vec();
            let name = String::from_utf8(name).unwrap();
            field_names.push(name);
        }

        field_names
    }

    /// Sets the value of a field.
    pub fn set(&self, id: usize, key: &str, val: &str) {
        if !self.exists(id) {
            return;
        }

        let key = format!("object-field-{id}-{key}");
        self.tree
            .insert(key.into_bytes(), val.to_string().into_bytes())
            .unwrap();
    }

    /// Gets the value of a field.
    pub fn get(&self, id: usize, key: &str) -> Option<String> {
        self.tree
            .get(format!("object-field-{id}-{key}"))
            .unwrap()
            .map(|val| String::from_utf8(val.to_vec()).unwrap())
    }

    /// Makes a server announcement.
    pub fn announce(&self, message: &str) {
        let _ = self.announcement_tx.send(message.to_string());
    }
}

#[derive(Default)]
pub struct Commands(HashMap<String, Command>);

impl Commands {
    pub fn new() -> Self {
        let mut cmds = Self::default();

        cmds.insert("say", say);
        cmds.insert("help", help);
        cmds.insert("@create", create);
        cmds.insert("@destroy", destroy);
        cmds.insert("@list", list);
        cmds.insert("@show", show);
        cmds.insert("@set", set);
        cmds.insert("@get", get);

        cmds
    }

    pub fn insert(&mut self, name: &str, cb: Command) {
        self.0.insert(name.to_string(), cb);
    }
}

pub struct User {
    pub state: Arc<State>,
    tx: UnboundedSender<String>,
    commands: Commands,
    quit: bool,
}

impl User {
    pub fn new(state: Arc<State>, mut tcp_tx: WriteHalf<TcpStream>) -> Self {
        let commands = Commands::new();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if tcp_tx.write_all(message.as_bytes()).await.is_err()
                    || tcp_tx.write_all(b"\r\n").await.is_err()
                {
                    break;
                }
            }
        });

        tokio::spawn({
            let tx = tx.clone();
            let mut rx = state.announcement_tx.subscribe();
            async move {
                while let Ok(message) = rx.recv().await {
                    if tx.send(message).is_err() {
                        break;
                    }
                }
            }
        });

        Self {
            state,
            tx,
            commands,
            quit: false,
        }
    }

    pub async fn run(mut self, rx: ReadHalf<TcpStream>) {
        self.message("Welcome to MarcieMOO!");

        let mut reader = BufReader::new(rx);
        let mut line_buf = String::new();

        while !self.quit {
            line_buf.clear();
            reader.read_line(&mut line_buf).await.unwrap();
            self.on_line(line_buf.as_str().trim()).await;
        }
    }

    pub async fn on_line(&mut self, line: &str) {
        let mut words = line.split(' ');
        let command = words.next().unwrap();
        let args: Vec<_> = words.map(ToString::to_string).collect();
        let args = Arguments::new(args);

        match self.commands.0.get(command) {
            Some(command) => {
                let result = command(self, args);
                if let Err(err) = result {
                    let msg = format!("error: {}", err);
                    self.message(&msg);
                }
            }
            None => {
                let msg = format!("command {:?} not found", command);
                self.message(&msg);
            }
        }
    }

    pub fn message(&mut self, text: &str) {
        if self.tx.send(text.to_string()).is_err() {
            self.quit = true;
        }
    }
}

pub enum CommandError {
    MissingArgument { index: usize },
    InvalidArgument { index: usize, expected: String },
}

impl Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::MissingArgument { index } => {
                write!(f, "missing argument at index {index}")
            }
            CommandError::InvalidArgument { index, expected } => {
                write!(f, "invalid argument at index {index} (expected {expected})")
            }
        }
    }
}

pub type CommandResult<T> = Result<T, CommandError>;

#[derive(Clone, Debug)]
pub enum Argument {
    Integer(i32),
    String(String),
    Ident(String),
}

pub struct Arguments(Vec<Argument>);

impl Arguments {
    pub fn new(words: Vec<String>) -> Self {
        let mut args = Vec::new();

        for word in words {
            if let Ok(val) = word.parse() {
                args.push(Argument::Integer(val));
                continue;
            }

            if !word.starts_with('\"') {
                args.push(Argument::Ident(word));
                continue;
            }

            // TODO better string parsing
            args.push(Argument::String(word));
        }

        Self(args)
    }

    pub fn get(&self, index: usize) -> CommandResult<Argument> {
        self.0
            .get(index)
            .cloned()
            .ok_or(CommandError::MissingArgument { index })
    }

    pub fn get_integer(&self, index: usize) -> CommandResult<i32> {
        match self.get(index)? {
            Argument::Integer(val) => Ok(val),
            _ => Err(CommandError::InvalidArgument {
                index,
                expected: "integer".to_string(),
            }),
        }
    }

    pub fn get_id(&self, index: usize) -> CommandResult<usize> {
        let id = self.get_integer(index)?;

        match id.try_into() {
            Ok(val) => Ok(val),
            Err(_) => Err(CommandError::InvalidArgument {
                index,
                expected: "object ID".to_string(),
            }),
        }
    }

    pub fn get_string(&self, index: usize) -> CommandResult<String> {
        match self.get(index)? {
            Argument::String(val) => Ok(val),
            _ => Err(CommandError::InvalidArgument {
                index,
                expected: "string".to_string(),
            }),
        }
    }

    pub fn get_ident(&self, index: usize) -> CommandResult<String> {
        match self.get(index)? {
            Argument::Ident(val) => Ok(val),
            _ => Err(CommandError::InvalidArgument {
                index,
                expected: "identifier".to_string(),
            }),
        }
    }
}

pub type Command = fn(&mut User, Arguments) -> CommandResult<()>;

pub fn say(user: &mut User, args: Arguments) -> CommandResult<()> {
    let say = args.get_string(0)?;
    let msg = format!("An unknown fellow says: {}", say);
    user.state.announce(&msg);
    Ok(())
}

pub fn help(user: &mut User, _args: Arguments) -> CommandResult<()> {
    user.message("Available commands:");

    let mut commands: Vec<_> = user.commands.0.keys().cloned().collect();
    commands.sort();

    for command in commands {
        user.message(&format!("    {command}"));
    }

    Ok(())
}

pub fn create(user: &mut User, _args: Arguments) -> CommandResult<()> {
    let idx = user.state.create();
    user.message(&format!("Created object #{idx}"));
    Ok(())
}

pub fn destroy(user: &mut User, _args: Arguments) -> CommandResult<()> {
    user.message("unimplemented");
    Ok(())
}

pub fn list(user: &mut User, _args: Arguments) -> CommandResult<()> {
    user.message("Objects:");

    for id in user.state.list() {
        user.message(&format!("    #{id}"));
    }

    Ok(())
}

pub fn show(user: &mut User, args: Arguments) -> CommandResult<()> {
    let id = args.get_id(0)?;

    if !user.state.exists(id) {
        user.message("No such object");
        return Ok(());
    }

    user.message(&format!("Fields on object #{id}"));

    for field in user.state.show(id) {
        user.message(&format!("    {field}"));
    }

    Ok(())
}

pub fn set(user: &mut User, args: Arguments) -> CommandResult<()> {
    let id = args.get_id(0)?;
    let key = args.get_ident(1)?;
    let val = args.get_string(2)?;

    if !user.state.exists(id) {
        user.message("No such object");
        return Ok(());
    }

    user.state.set(id, &key, &val);

    Ok(())
}

pub fn get(user: &mut User, args: Arguments) -> CommandResult<()> {
    let id = args.get_id(0)?;
    let key = args.get_ident(1)?;

    match user.state.get(id, &key) {
        Some(val) => user.message(&format!("value: {:?}", val)),
        None => user.message("value: <none>"),
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("0.0.0.0:8888").await.unwrap();

    let state = State::default();
    let state = Arc::new(state);

    while let Ok((conn, addr)) = listener.accept().await {
        eprintln!("Connection from {addr}");

        let (rx, tx) = tokio::io::split(conn);
        let user = User::new(state.clone(), tx);

        tokio::spawn(async move {
            user.run(rx).await;
            eprintln!("{addr} disconnected");
        });
    }
}
