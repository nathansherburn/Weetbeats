/*
 * Checks the front end and Rust agree about what the commands are called and what they take.
 *
 *   node ui/test/contract.js
 *
 * The front end talks to Rust through strings, so a renamed command or argument is a runtime
 * error in a place nothing else looks. This reads both sides and compares them.
 *
 * It cannot check what a command *returns*, which is the one thing here that has actually
 * gone wrong: a command was changed to hand back a pair, the front end was changed to unpack
 * one, and the Rust change never landed. The front end's tests passed because the stub had
 * been changed too. What catches that is the unhandled rejection handler in main.js, which
 * puts the error where you can see it instead of leaving the window looking ignored.
 */
const fs = require("fs");
const path = require("path");

const root = path.resolve(__dirname, "../..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");

const main = read("ui/main.js");
const shim = read("ui/test/shim.js");
const commands = read("src-tauri/src/commands.rs");
const wiring = read("src-tauri/src/main.rs");

const problems = [];
const fail = (why) => problems.push(why);

/* Split a comma separated list without cutting inside <>, () or {}. */
function topLevel(text, [open, close] = ["<([{", ">)]}"]) {
  const parts = [];
  let depth = 0;
  let from = 0;
  for (let i = 0; i < text.length; i++) {
    if (open.includes(text[i])) depth += 1;
    else if (close.includes(text[i])) depth -= 1;
    else if (text[i] === "," && depth === 0) {
      parts.push(text.slice(from, i));
      from = i + 1;
    }
  }
  parts.push(text.slice(from));
  return parts.map((one) => one.trim()).filter(Boolean);
}

/* The braces of the object literal starting at `at`, whatever is nested inside it. */
function objectAt(text, at) {
  let depth = 0;
  for (let i = at; i < text.length; i++) {
    if (text[i] === "{") depth += 1;
    if (text[i] === "}") {
      depth -= 1;
      if (depth === 0) return text.slice(at + 1, i);
    }
  }
  return "";
}

// What Rust exposes: the handler list in main.rs is the only thing that makes a function
// reachable, so that is the list that counts.
const wired = [...wiring.matchAll(/commands::(\w+)/g)].map((m) => m[1]);

// And what each of those takes. Tauri's own arguments are not the front end's business.
const params = new Map();
for (const match of commands.matchAll(
  /#\[tauri::command\]\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)\s*\(([\s\S]*?)\)\s*(?:->|\{)/g,
)) {
  const [, name, args] = match;
  const taken = topLevel(args)
    .filter((one) => !/\bState\s*<|\bAppHandle\b/.test(one))
    .map((one) => one.split(":")[0].trim());
  params.set(name, taken);
}

for (const name of wired) {
  if (!params.has(name)) fail(`main.rs wires up commands::${name}, which is not a command`);
}

// Every call the front end makes by name, and the arguments it passes where they are written
// out. Some are made through a helper that takes the name as an argument — those still count
// as called, they just have nothing to check the arguments against here.
const called = new Set();
const calls = [];
for (const match of main.matchAll(/"(\w+)"/g)) {
  if (params.has(match[1])) called.add(match[1]);
}
for (const match of main.matchAll(/invoke\(\s*"(\w+)"\s*(,\s*\{)?/g)) {
  const name = match[1];
  const keys = match[2]
    ? topLevel(objectAt(main, main.indexOf("{", match.index + match[0].length - 2))).map((one) =>
        one.split(":")[0].trim(),
      )
    : [];
  calls.push({ name, keys });
}

for (const call of calls) {
  if (!params.has(call.name)) {
    fail(`main.js calls "${call.name}", which Rust does not have`);
    continue;
  }
  if (!wired.includes(call.name)) {
    fail(`main.js calls "${call.name}", which main.rs never wires up`);
  }
  for (const key of call.keys) {
    if (!params.get(call.name).includes(key)) {
      fail(
        `main.js calls "${call.name}" with { ${key} }, which it does not take ` +
          `(it takes ${params.get(call.name).join(", ") || "nothing"})`,
      );
    }
  }
}

// The stub the front end tests run against has to answer everything the real one does, or a
// passing test says nothing about the real app.
const stubbed = new Set(
  [...shim.matchAll(/^\s{2}(\w+)\s*[:,]/gm)].map((m) => m[1]),
);
for (const name of wired) {
  if (!stubbed.has(name)) fail(`the test shim has no ${name}, so the tests never exercise it`);
  if (!called.has(name)) fail(`Rust exposes ${name}, which the front end never calls`);
}

for (const problem of problems) {
  console.log(` FAIL  ${problem}`);
}
console.log(
  problems.length
    ? `\n${problems.length} mismatch${problems.length === 1 ? "" : "es"} between the front end and Rust`
    : `  ok   ${calls.length} calls across ${wired.length} commands, all agreed`,
);
process.exit(problems.length ? 1 : 0);
