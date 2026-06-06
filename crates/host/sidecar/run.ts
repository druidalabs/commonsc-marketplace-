// Sidecar entry. Runs under Deno with the permission set the Rust host hands us;
// loads Pyodide; speaks newline-delimited JSON over stdin/stdout to the parent.
//
// Privacy invariant: this file does not import anything that touches the network,
// the filesystem outside Deno's own npm cache, or subprocesses. Adding such an
// import would expand the capability surface and must be reviewed against the
// brief's §3 invariants.

import { loadPyodide, type PyodideInterface } from "npm:pyodide@0.26.2";

type Command =
  | { type: "hello"; expr: string }
  | {
      type: "run";
      bundleDir: string;
      module: string;
      function: string;
      variantSet: unknown;
    }
  | { type: "shutdown" };

type Event =
  | { type: "ready" }
  | { type: "result"; value: unknown }
  | { type: "progress"; percent: number; label?: string }
  | { type: "log"; level: "debug" | "info" | "warn"; message: string }
  | { type: "error"; message: string };

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function emit(event: Event): void {
  // Single write call per event keeps the parent's line-based reader happy even
  // under concurrent emissions (e.g. progress arriving mid-result).
  const line = JSON.stringify(event) + "\n";
  Deno.stdout.writeSync(encoder.encode(line));
}

async function* readCommands(): AsyncGenerator<Command> {
  let buffer = "";
  for await (const chunk of Deno.stdin.readable) {
    buffer += decoder.decode(chunk, { stream: true });
    let nl: number;
    while ((nl = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, nl).trim();
      buffer = buffer.slice(nl + 1);
      if (line.length === 0) continue;
      yield JSON.parse(line) as Command;
    }
  }
}

/**
 * Recursively copy every file from the Rust-unpacked bundle directory into
 * Pyodide's virtual FS at `dstRoot`. The bundle is small (algorithm source +
 * weights), so a single eager copy is fine — streaming becomes worthwhile when
 * algorithms ship larger reference data, at which point we'd switch to
 * pyodide.FS.mount with a Node-style backend.
 */
async function mountBundleInto(
  pyodide: PyodideInterface,
  srcAbsDir: string,
  dstRoot: string,
): Promise<void> {
  pyodide.FS.mkdirTree(dstRoot);
  for await (const entry of Deno.readDir(srcAbsDir)) {
    const srcPath = `${srcAbsDir}/${entry.name}`;
    const dstPath = `${dstRoot}/${entry.name}`;
    if (entry.isDirectory) {
      pyodide.FS.mkdirTree(dstPath);
      await mountBundleInto(pyodide, srcPath, dstPath);
    } else if (entry.isFile) {
      const bytes = await Deno.readFile(srcPath);
      pyodide.FS.writeFile(dstPath, bytes);
    }
  }
}

async function main(): Promise<void> {
  let pyodide: PyodideInterface;
  try {
    pyodide = await loadPyodide();
  } catch (err) {
    emit({ type: "error", message: `pyodide failed to load: ${String(err)}` });
    Deno.exit(1);
  }

  emit({ type: "ready" });

  for await (const cmd of readCommands()) {
    switch (cmd.type) {
      case "hello": {
        try {
          const raw = pyodide.runPython(cmd.expr);
          const value =
            raw && typeof raw === "object" && "toJs" in raw
              ? (raw as { toJs(): unknown }).toJs()
              : raw;
          emit({ type: "result", value: value as unknown });
        } catch (err) {
          emit({ type: "error", message: String(err) });
        }
        break;
      }
      case "run": {
        try {
          // 1. Pyodide is a fresh interpreter for this sidecar invocation, but
          //    the FS layer is shared across run commands within one process.
          //    A unique mount point per run keeps modules from leaking.
          const mountPoint = `/algorithm/${crypto.randomUUID()}`;
          await mountBundleInto(pyodide, cmd.bundleDir, mountPoint);

          // 2. Make the unpacked dir importable, then import + call the
          //    declared entrypoint.
          pyodide.runPython(
            `import sys\nsys.path.insert(0, ${JSON.stringify(mountPoint)})`,
          );
          const mod = pyodide.pyimport(cmd.module);
          const fn = mod[cmd.function];
          if (typeof fn !== "function") {
            emit({
              type: "error",
              message: `entrypoint ${cmd.module}.${cmd.function} is not callable`,
            });
            break;
          }
          // pyodide.toPy converts the JS variant_set to a Python dict the
          // algorithm can iterate. The return value's .toJs() unwraps nested
          // PyProxy containers into plain JS so JSON.stringify works.
          const pyInput = pyodide.toPy(cmd.variantSet);
          let raw: unknown;
          try {
            raw = fn(pyInput);
          } finally {
            // PyProxies must be released; otherwise pyodide leaks memory in
            // the wasm heap (Python GC can't reach them through JS refs).
            if (pyInput && typeof (pyInput as { destroy?: () => void }).destroy === "function") {
              (pyInput as { destroy: () => void }).destroy();
            }
          }
          const value = (() => {
            if (raw && typeof raw === "object" && "toJs" in raw) {
              const js = (raw as { toJs(opts: unknown): unknown }).toJs({
                dict_converter: Object.fromEntries,
              });
              if (typeof (raw as { destroy?: () => void }).destroy === "function") {
                (raw as { destroy: () => void }).destroy();
              }
              return js;
            }
            return raw;
          })();
          emit({ type: "result", value });
        } catch (err) {
          emit({ type: "error", message: `algorithm threw: ${String(err)}` });
        }
        break;
      }
      case "shutdown": {
        return;
      }
      default: {
        emit({
          type: "error",
          message: `unknown command type: ${(cmd as { type: string }).type}`,
        });
      }
    }
  }
}

await main();
