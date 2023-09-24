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

- [ ] `get` command
- [ ] licensing
- [ ] server announcements
- [ ] `say` command
- [ ] base classes
- [ ] rooms
- [ ] `look` command
- [ ] `help <command>` support
- [ ] user management (creation, deletion, passwords)
- [ ] `login` command
- [ ] Rhai scripting
- [ ] verbs
- [ ] permissions?
- [ ] use argon2 for passwords
