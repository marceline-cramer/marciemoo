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

- verb arguments
- figure out verb definition UX
- code editor for verbs
- licensing
- base classes
- more sandboxed Rhai imports
- containers
- rooms
- command documentation
- document existing commands
- `help <command>` support
- user management (creation, deletion, passwords)
- use argon2 for passwords
- `login` command
- programmers vs. wizards vs. users
- permissions
