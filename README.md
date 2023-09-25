# MarcieMOO

Minimalist MOO-like server.

Run with `cargo run`.

Connect with a MUD client like BlightMud or with telnet:
```bash
$ blightmud -c 127.0.0.1:8888
$ # OR
$ telnet 127.0.0.1 8888
```

# To-Do

- licensing
- base classes
- rooms
- command documentation
- documented commands
- `help <command>` support
- permissions?
- verb arguments
- figure out verb definition UX
- code editor for verbs
- user management (creation, deletion, passwords)
- `login` command
- use argon2 for passwords
