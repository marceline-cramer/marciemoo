use std::{collections::HashMap, fmt::Display, sync::Arc};

use sled::Tree;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::mpsc::UnboundedSender,
};

pub struct State {
    tree: Tree,
}

impl Default for State {
    fn default() -> Self {
        let db = sled::open("marciemoo.db").unwrap();
        let tree = db.open_tree("").unwrap();
        Self { tree }
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
    ///
    /// Returns `false` if the ID was invalid.
    pub fn set(&self, id: usize, key: &str, val: &str) -> bool {
        if !self
            .tree
            .contains_key(format!("object-exists-{id}"))
            .unwrap()
        {
            return false;
        }

        let key = format!("object-field-{id}-{key}");
        self.tree
            .insert(key.into_bytes(), val.to_string().into_bytes())
            .unwrap();

        true
    }
}

#[derive(Default)]
pub struct Commands(HashMap<String, Command>);

impl Commands {
    pub fn new() -> Self {
        let mut cmds = Self::default();

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
}

impl User {
    pub fn new(state: Arc<State>, mut tcp_tx: WriteHalf<TcpStream>) -> Self {
        let commands = Commands::new();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                tcp_tx.write_all(message.as_bytes()).await.unwrap();
                tcp_tx.write_all(b"\r\n").await.unwrap();
            }
        });

        Self {
            state,
            tx,
            commands,
        }
    }

    pub async fn run(mut self, rx: ReadHalf<TcpStream>) {
        self.message("Welcome to MarcieMOO!");

        let mut reader = BufReader::new(rx);
        let mut line_buf = String::new();

        loop {
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

    pub fn message(&self, text: &str) {
        self.tx.send(text.to_string()).unwrap();
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

            unimplemented!("string arguments");
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

pub fn help(user: &mut User, args: Arguments) -> CommandResult<()> {
    user.message("Available commands:");
    for command in user.commands.0.keys() {
        user.message(&format!("    {command}"));
    }

    Ok(())
}

pub fn create(user: &mut User, _args: Arguments) -> CommandResult<()> {
    let idx = user.state.create();
    user.message(&format!("Created object #{idx}"));
    Ok(())
}

pub fn destroy(user: &mut User, args: Arguments) -> CommandResult<()> {
    user.message("unimplemented");
    Ok(())
}

pub fn list(user: &mut User, args: Arguments) -> CommandResult<()> {
    user.message("unimplemented");
    Ok(())
}

pub fn show(user: &mut User, args: Arguments) -> CommandResult<()> {
    user.message("unimplemented");
    Ok(())
}

pub fn set(user: &mut User, args: Arguments) -> CommandResult<()> {
    user.message("unimplemented");
    Ok(())
}

pub fn get(user: &mut User, args: Arguments) -> CommandResult<()> {
    user.message("unimplemented");
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
