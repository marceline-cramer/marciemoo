use std::{collections::HashMap, fmt::Display, net::SocketAddr, sync::Arc};

use logos::Logos;
use serde::{Deserialize, Serialize};
use sled::Tree;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc::UnboundedSender},
};
use tokio_util::sync::CancellationToken;

pub mod script;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Value {
    String(String),
    Integer(i64),
    Bool(bool),
}

impl Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(val) => write!(f, "{:?}", val),
            Value::Integer(val) => write!(f, "{}", val),
            Value::Bool(val) => write!(f, "{}", val),
        }
    }
}

impl Value {
    pub fn as_string(&self) -> Option<&String> {
        match self {
            Value::String(val) => Some(val),
            _ => None,
        }
    }
}

pub struct State {
    tree: Tree,
    shutdown: CancellationToken,
    announcement_tx: broadcast::Sender<String>,
}

impl State {
    pub fn new(shutdown: CancellationToken) -> Self {
        let db = sled::open("marciemoo.db").unwrap();
        let tree = db.open_tree("").unwrap();
        let announcement_tx = broadcast::Sender::new(1024);

        Self {
            tree,
            shutdown,
            announcement_tx,
        }
    }

    /// Retrieves a child [CancellationToken] for this state.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.child_token()
    }

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

    /// Atomically destroys an object by ID.
    pub fn destroy(&self, id: usize) -> bool {
        let key = format!("object-exists-{id}");

        if self.tree.remove(key).unwrap().is_none() {
            // either this object is already destroyed or another thread is
            // currently destroying it, so we can exit
            return false;
        }

        let prefix = format!("object-field-{id}-");
        for field in self.tree.scan_prefix(&prefix) {
            self.tree.remove(field.unwrap().0).unwrap();
        }

        true
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

    /// Shows all the fields on an object.
    pub fn show(&self, id: usize) -> Vec<(String, Value)> {
        let prefix = format!("object-field-{id}-");
        let prefix_len = prefix.len();
        let field_iter = self.tree.scan_prefix(prefix.into_bytes());

        let mut fields = Vec::new();
        for field in field_iter {
            let (key, value) = field.unwrap();
            let key = key[prefix_len..].to_vec();
            let key = String::from_utf8(key).unwrap();
            let value = serde_json::from_slice(&value).unwrap();
            fields.push((key, value));
        }

        fields
    }

    /// Sets the value of a field.
    pub fn set(&self, id: usize, key: &str, val: Value) {
        if !self.exists(id) {
            return;
        }

        let key = format!("object-field-{id}-{key}");
        let val = serde_json::to_vec(&val).unwrap();
        self.tree.insert(key.into_bytes(), val).unwrap();
    }

    /// Gets the value of a field.
    pub fn get(&self, id: usize, key: &str) -> Option<Value> {
        let val = self.tree.get(format!("object-field-{id}-{key}")).unwrap()?;
        let val = serde_json::from_slice(&val).unwrap();
        Some(val)
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
    object: usize,
    tx: UnboundedSender<String>,
    commands: Commands,
    quit: bool,
}

impl User {
    pub fn new(state: Arc<State>, mut tcp_tx: WriteHalf<TcpStream>) -> Self {
        let commands = Commands::new();
        let object = state.create();

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
            object,
        }
    }

    pub async fn run(mut self, rx: ReadHalf<TcpStream>) {
        self.message("Welcome to MarcieMOO!");
        self.message(&format!("You are object #{}.", self.object));

        let mut reader = BufReader::new(rx);
        let mut line_buf = String::new();
        let shutdown = self.state.shutdown_token();

        while !self.quit {
            line_buf.clear();

            tokio::select! {
                _ = shutdown.cancelled() => {
                    self.quit = true;
                }
                result = reader.read_line(&mut line_buf) => {
                    if result.is_err() {
                        self.quit = true;
                    } else {
                        self.on_line(line_buf.as_str().trim()).await;
                    }
                }
            };
        }

        self.state.destroy(self.object);
    }

    pub async fn on_line(&mut self, line: &str) {
        let (command, args) = line.split_once(' ').unwrap_or((line, ""));

        match self.commands.0.get(command) {
            Some(command) => {
                if let Err(err) = self.exec_command(*command, args) {
                    let msg = format!("error: {}", err);
                    self.message(&msg);
                }
            }
            None => {
                self.exec(command);
            }
        }
    }

    pub fn exec_command(&mut self, command: Command, args: &str) -> CommandResult<()> {
        let args = Arguments::new(args)?;
        command(self, args)?;
        Ok(())
    }

    pub fn message(&mut self, text: &str) {
        if self.tx.send(text.to_string()).is_err() {
            self.quit = true;
        }
    }

    /// Executes a verb.
    pub fn exec(&mut self, verb: &str) {
        let messages = self
            .state
            .tree
            .transaction::<_, _, ()>(|tx| {
                let key = format!("object-field-{}-{verb}", self.object);
                let Some(val) = tx.get(key)? else {
                    return Ok(vec![format!("no such verb")]);
                };

                let val = serde_json::from_slice(&val).unwrap();
                let Value::String(src) = val else {
                    return Ok(vec![format!("field is not executable")]);
                };

                let runtime = script::Runtime::new(tx, self.object);
                let messages = runtime.run(&src)?;
                Ok(messages)
            })
            .unwrap();

        for message in messages {
            self.message(&message);
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

#[derive(Clone, Debug, Logos)]
#[logos(skip r" +")]
pub enum ArgumentKind {
    #[regex("[0-9]+")]
    Integer,

    #[regex("\"[^\"]*\"")]
    String,

    #[regex("[a-zA-Z_]+")]
    Ident,

    #[token("false")]
    False,

    #[token("true")]
    True,
}

#[derive(Clone, Debug)]
pub enum Argument {
    Bool(bool),
    Integer(i64),
    String(String),
    Ident(String),
}

pub struct Arguments(Vec<Argument>);

impl Arguments {
    pub fn new(words: &str) -> CommandResult<Self> {
        let mut lexer = ArgumentKind::lexer(words);
        let mut args = Vec::new();

        while let Some(arg) = lexer.next() {
            let index = args.len();

            let arg = if let Ok(arg) = arg {
                arg
            } else {
                return Err(CommandError::InvalidArgument {
                    index,
                    expected: "argument".to_string(),
                });
            };

            let slice = lexer.slice();
            args.push(match arg {
                ArgumentKind::Integer => Argument::Integer(slice.parse().unwrap()),
                ArgumentKind::String => Argument::String(slice[1..slice.len() - 1].to_string()),
                ArgumentKind::Ident => Argument::Ident(slice.to_owned()),
                ArgumentKind::False => Argument::Bool(false),
                ArgumentKind::True => Argument::Bool(true),
            });
        }

        Ok(Self(args))
    }

    pub fn get(&self, index: usize) -> CommandResult<Argument> {
        self.0
            .get(index)
            .cloned()
            .ok_or(CommandError::MissingArgument { index })
    }

    pub fn get_value(&self, index: usize) -> CommandResult<Value> {
        match self.get(index)? {
            Argument::Integer(val) => Ok(Value::Integer(val)),
            Argument::Bool(val) => Ok(Value::Bool(val)),
            Argument::String(val) => Ok(Value::String(val)),
            _ => Err(CommandError::InvalidArgument {
                index,
                expected: "value".to_string(),
            }),
        }
    }

    pub fn get_integer(&self, index: usize) -> CommandResult<i64> {
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

    let who = match user
        .state
        .get(user.object, "name")
        .and_then(|name| name.as_string().cloned())
    {
        Some(name) => name,
        None => format!("#{}", user.object),
    };

    let msg = format!("{who} says: {say}");
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
    user.message(&format!("created object #{idx}"));
    Ok(())
}

pub fn destroy(user: &mut User, args: Arguments) -> CommandResult<()> {
    let idx = args.get_id(0)?;

    if user.state.destroy(idx) {
        user.message("success");
    } else {
        user.message("no such object");
    }

    Ok(())
}

pub fn list(user: &mut User, _args: Arguments) -> CommandResult<()> {
    user.message("Objects:");

    for id in user.state.list() {
        let msg = match user
            .state
            .get(id, "name")
            .and_then(|name| name.as_string().cloned())
        {
            Some(name) => format!("    #{:<4} ({})", id, name),
            None => format!("    #{}", id),
        };

        user.message(&msg);
    }

    Ok(())
}

pub fn show(user: &mut User, args: Arguments) -> CommandResult<()> {
    let id = args.get_id(0)?;

    if !user.state.exists(id) {
        user.message("no such object");
        return Ok(());
    }

    user.message(&format!("Fields on object #{id}"));

    for (key, val) in user.state.show(id) {
        user.message(&format!("    {:<20}{}", key, val));
    }

    Ok(())
}

pub fn set(user: &mut User, args: Arguments) -> CommandResult<()> {
    let id = args.get_id(0)?;
    let key = args.get_ident(1)?;
    let val = args.get_value(2)?;

    if !user.state.exists(id) {
        user.message("No such object");
        return Ok(());
    }

    user.state.set(id, &key, val);

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
    let bind = "0.0.0.0:8888";
    let listener = TcpListener::bind(bind).await.unwrap();
    eprintln!("Listening on {bind}");

    let token = CancellationToken::new();
    let state = State::new(token.child_token());
    let state = Arc::new(state);

    let shutdown = token.child_token();
    tokio::spawn(wait_for_interrupt(token));

    loop {
        tokio::select! {
            incoming = listener.accept() => {
                let (conn, addr) = incoming.unwrap();
                accept(state.clone(), conn, addr);
            }
            _ = shutdown.cancelled() => {
                break;
            }
        }
    }
}

fn accept(state: Arc<State>, conn: TcpStream, addr: SocketAddr) {
    eprintln!("Connection from {addr}");
    let (rx, tx) = tokio::io::split(conn);
    let user = User::new(state, tx);

    tokio::spawn(async move {
        user.run(rx).await;
        eprintln!("{addr} disconnected");
    });
}

async fn wait_for_interrupt(shutdown: CancellationToken) {
    tokio::signal::ctrl_c().await.unwrap();
    shutdown.cancel();
}
