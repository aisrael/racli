# queue
Queue is a small “learning” project for Rust that implements a task spooler like utility written in Rust.

It can launch in daemon mode, listen on a named piped, and execute any commands sent to that pipe.

Obviously insecure and nowhere near ‘production-ready’, but I wrote it to get a good feel of Rust beyond the syntax/semantics and into actual operational stuff.

## Building

```
$ cargo build
```

## Running

```
$ cargo run
```

Or

```
$ target/debug/queue
```

The first time you run it, queue launches in 'listener' mode and opens a FIFO or named pipe at `/tmp/queue`.

You can then run queue again (in a different shell session) and any arguments are passed on to the active listener. The listener accepts the arguments and executes them as a command.

Alternatively, you can simply `echo` or `cat` to the named piped at `/tmp/queue`.

### Examples

```
$ cargo run echo hello
$ echo 'echo hello' >> /tmp/queue
```

In both of the above examples, the listener should print `hello` to the console.
