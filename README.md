# sa — a terminal superagent that runs your other agents

`sa` is a single Rust binary that puts one agent in a terminal with real shell access, a live
map of your machine, and the ability to dispatch Claude Code, Codex and Pi — with a UI shaped
like Claude Code: amber `✻`, `⏺ tool(args)` / `⎿ result` pairs, rotating thinking verbs, a
boxed input, and a sidebar that shows every agent's state.

```
✻ SUPERAGENT  deepseek-flash    ASK  [AGENT]  DISPATCH  SYSTEM        $0.0091  12 steps
╭────────────────────────────────────────╮          agents
│ ✻ Welcome to SUPERAGENT                │         │ ● claude  done   42s
│   /map     system graph and wiring     │         │ ✻ pi      working
│   /agents  installed agent CLIs        │         │system
╰────────────────────────────────────────╯         │ 146 nodes · 174 edges
> which module owns the ingest retry loop?          │session
⏺ graph(ingest retry backoff)                       │ AGENT · yolo
  ⎿  ## System graph (146 nodes)                    │ $0.0091 · 0:51
⏺ src/ingest/worker.rs owns it: retries with...
  4 steps · 15.6k in / 623 out · $0.0022
╭────────────────────────────────────────╮
│❯                                       │
╰────────────────────────────────────────╯
╭──────────────────╮  ? for shortcuts              agent · yolo · esc clear
│ ✻ sa      ◉ live │
│ ▁▂▃▅▇▅▃▂▁▂▃▅▇▅▃▂ │
│ agent       yolo │
╰──────────────────╯
```

## What it does

- **One agent, many tools.** `sh`, `read`, `write`, `patch`, `graph`, `system_map`, `scan`,
  `graph_build`, `brain`, `agent`, `job`, `plan`.
- **Dispatches the others.** `agent` runs `claude -p`, `codex exec` or `pi -p` headlessly and
  returns what came back; the sidebar tracks each one live.
- **Maps the machine** (optional). If `sg` — a systemgraph CLI — is on `PATH`, `graph` answers
  questions across every registered repo plus a graph of the machine itself (bins, repos, jobs,
  MCP servers, model providers). Without it those three tools simply report that no graph exists;
  everything else works.
- **Remembers.** `brain` shells out to a local knowledge-vault CLI if one is installed.
- **Modes.** `ask` is read-only; `agent` is the full operator; `dispatch` plans and delegates;
  `system` maintains the map. Mode is enforced twice — the tool schema sent to the model is
  trimmed *and* execution refuses out-of-mode calls.

## Install

Requires Rust 1.75+ and a DeepSeek API key.

```bash
git clone https://github.com/Jared-boop44234/sa && cd sa
cargo build --release
ln -sf "$PWD/target/release/sa" ~/.local/bin/sa      # any PATH dir
export DEEPSEEK_API_KEY=...                          # or a key file, see below
sa
```

The key is read from `DEEPSEEK_API_KEY`, or from `~/.pi/agent/auth.json` if you use Pi
(`{"deepseek": {"key": "..."}}`). It is never printed, logged, or written to a transcript.

### macOS note

If linking fails with `tapi error: malformed file` / `unknown architecture`, the installed SDK
stub lacks an arm64 slice. Build against an older SDK:

```bash
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.sdk cargo build --release
```

## Configure it for you

Nothing about the operator is compiled in. Two files under `~/.config/sa` (or `$SA_HOME`):

```ini
# ~/.config/sa/config
operator = Your Name
```

```markdown
<!-- ~/.config/sa/rules.md — replaces the built-in safety rules verbatim -->
- Never move money or sign a contract.
- ...
```

`sa --show-prompt` prints the exact system prompt it would send, so you can see what the model
sees. Environment: `SA_MODEL`, `SA_EFFORT`, `SA_MODE`, `SA_RULES`, `SA_HOME`, `SA_OPERATOR`,
`SG_BIN`, `BRAIN_BIN`, `SA_NO_MAXIMIZE`.

## Use

```bash
sa                          # full-screen UI
sa "question"               # one-shot
sa -p "question"            # one-shot, non-interactive
sa --classic                # plain line REPL
sa --mode ask|agent|dispatch|system
sa --model deepseek-v4-pro --effort high
sa --ask                    # confirm writes/shell (yolo is the default)
sa --check | --map | --agents | --sessions | --show-prompt
```

Inside the UI: `⏎` send · `F1-F4` mode · `F5` sidebar · `esc` interrupt · `^C` interrupt ·
`^L` clear · `^T` reasoning · `^N` new session · `^D` quit · `?` shortcuts · `!cmd` shell.

Slash commands: `/map /roots /agents /graph /scan /build /jobs /cost /model /mode /cd /full
/think /new /session /clear /exit`.

## Permission model

| mode | shell | writes | dispatch |
|---|---|---|---|
| `agent` (default, yolo) | anything | anything | yes |
| `--ask` | prompts on risky patterns | prompts outside `$HOME` and `/tmp` | prompts |
| `ask` mode (read-only) | no | no | no |

`yolo` is deliberately unrestricted, the equivalent of `--dangerously-skip-permissions`: the
shell is the shell. The **model** is still told not to exfiltrate secrets or move money, and every
tool result is passed through a redactor (API keys, JWTs, `key=value` secrets, long hex) before it
reaches the model, the screen, or the session log.

## Architecture

```
src/main.rs    arg parsing, one-shot + classic REPL, window maximisation
src/llm.rs     DeepSeek chat completions over SSE, streaming deltas + tool-call assembly
src/agent.rs   the turn loop; emits Ev events so any front end can render the same run
src/tools.rs   tool schemas, execution, modes, approval, per-agent live status
src/prompt.rs  system prompt + local rule overlay + live inventory
src/ui.rs      ratatui screen: header, transcript, sidebar, camera tile, input box
src/config.rs  paths, key resolution, roots, agents
src/util.rs    redaction, truncation, small formatting
```

The agent runs on a worker thread and owns itself; the UI sends `Cmd` and receives `Ev` over
channels. That is why streaming never fights the renderer, and why a second front end (say a web
UI) is ~200 lines.

Sessions land in `~/.systemgraph/sessions/*.jsonl` (or `$SYSTEMGRAPH_HOME`), one JSON object per
message, tool results included — greppable, replayable, no database.

## License

MIT.
