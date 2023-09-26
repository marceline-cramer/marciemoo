use std::{
    rc::Rc,
    str::FromStr,
    sync::{Arc, Mutex},
};

use rhai::{Dynamic, Engine, EvalAltResult, Scope};
use sled::transaction::{TransactionalTree, UnabortableTransactionError};

use crate::Value;

type Error = Rc<Mutex<Option<UnabortableTransactionError>>>;

#[derive(Clone)]
pub struct Object {
    id: usize,
    tx: &'static TransactionalTree,
    error: Error,
}

impl Object {
    fn get(&mut self, field: &str) -> Result<Dynamic, Box<EvalAltResult>> {
        let key = format!("object-field-{}-{}", self.id, field);
        let val = match self.tx.get(key) {
            Ok(val) => val,
            Err(err) => {
                let _ = self.error.lock().unwrap().insert(err);
                return Err(Box::new("transaction error".into()));
            }
        };

        let Some(val) = val else {
            return Ok(Dynamic::UNIT);
        };

        let val: Value = serde_json::from_slice(&val).unwrap();
        let val = match val {
            Value::Integer(val) => Dynamic::from_int(val),
            Value::String(val) => Dynamic::from_str(&val).unwrap(),
            Value::Bool(val) => Dynamic::from_bool(val),
        };

        Ok(val)
    }

    fn set(&mut self, field: &str, val: Dynamic) -> Result<(), Box<EvalAltResult>> {
        let key = format!("object-field-{}-{field}", self.id);

        if val.is_unit() {
            match self.tx.remove(key.into_bytes()) {
                Ok(_) => return Ok(()),
                Err(err) => {
                    let _ = self.error.lock().unwrap().insert(err);
                    return Err(Box::new("transaction error".into()));
                }
            }
        }

        let val = if val.is_string() {
            Value::String(val.into_string().unwrap())
        } else if val.is_int() {
            Value::Integer(val.as_int().unwrap())
        } else if val.is_bool() {
            Value::Bool(val.as_bool().unwrap())
        } else {
            return Err(Box::new("invalid value type".into()));
        };

        let val = serde_json::to_vec(&val).unwrap();
        let result = self.tx.insert(key.into_bytes(), val);

        if let Err(err) = result {
            let _ = self.error.lock().unwrap().insert(err);
            return Err(Box::new("transaction error".into()));
        }

        Ok(())
    }
}

pub struct Runtime {
    engine: Engine,
    self_object: Object,
    output: Arc<Mutex<ScriptOutput>>,
}

impl Runtime {
    pub fn new(tx: &TransactionalTree, self_id: usize) -> Self {
        let tx: &'static TransactionalTree = unsafe { std::mem::transmute(tx) };
        let error = Error::default();

        let mut engine = Engine::new_raw();
        let output: Arc<Mutex<ScriptOutput>> = Default::default();

        engine
            .register_type::<Object>()
            .register_indexer_get(Object::get)
            .register_indexer_set(Object::set);

        engine.register_fn("object", {
            let error = error.clone();
            move |id| {
                Dynamic::from(Object {
                    id,
                    tx,
                    error: error.clone(),
                })
            }
        });

        engine.register_fn("print", {
            let output = output.clone();
            move |message: String| {
                output.lock().unwrap().messages.push(message);
            }
        });

        engine.register_fn("announce", {
            let output = output.clone();
            move |message: String| {
                output.lock().unwrap().announcements.push(message);
            }
        });

        let self_object = Object {
            id: self_id,
            tx,
            error,
        };

        Self {
            engine,
            output,
            self_object,
        }
    }

    pub fn run(&self, src: &str) -> Result<ScriptOutput, UnabortableTransactionError> {
        self.self_object.error.lock().unwrap().take();

        let mut scope = Scope::new();
        scope.set_value("self", self.self_object.clone());

        let result = self.engine.eval_with_scope::<()>(&mut scope, src);

        if let Some(err) = self.self_object.error.lock().unwrap().take() {
            return Err(err);
        }

        let mut output: ScriptOutput = self.output.lock().unwrap().to_owned();

        if let Some(err) = result.err() {
            output.messages.push(format!("script error: {}", err));
        }

        Ok(output)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScriptOutput {
    /// Messages addressed to the subject.
    pub messages: Vec<String>,

    /// Server-wide announcements.
    pub announcements: Vec<String>,
}

impl ScriptOutput {
    pub fn message(message: impl ToString) -> Self {
        Self {
            messages: vec![message.to_string()],
            ..Default::default()
        }
    }
}
