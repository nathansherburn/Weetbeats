/*
 * Drives the front end in a real browser with the Rust side stubbed out, so the parts
 * that only exist inside a webview — the canvas grid, drag painting, the keyboard — are
 * actually exercised. Needs Playwright:
 *
 *   npm install playwright && npx playwright install chromium
 *   node ui/test/run.js
 */
const { chromium } = require("playwright");
const fs = require("fs");
const path = require("path");
const http = require("http");

const UI = path.resolve(__dirname, "..");
const SHIM = fs.readFileSync(path.join(__dirname, "shim.js"), "utf8");

const types = { ".html": "text/html", ".css": "text/css", ".js": "text/javascript" };

const server = http.createServer((req, res) => {
  const file = path.join(UI, req.url === "/" ? "index.html" : req.url.split("?")[0]);
  if (!file.startsWith(UI) || !fs.existsSync(file)) {
    res.writeHead(404).end("nope");
    return;
  }
  res.writeHead(200, { "content-type": types[path.extname(file)] || "text/plain" });
  res.end(fs.readFileSync(file));
});

const checks = [];
const check = (name, ok, detail = "") => {
  checks.push({ name, ok, detail });
  console.log(`${ok ? "  ok  " : " FAIL "} ${name}${detail ? " — " + detail : ""}`);
};

(async () => {
  await new Promise((r) => server.listen(0, r));
  const url = `http://127.0.0.1:${server.address().port}/`;

  // Honour an explicit browser path when there is one; otherwise let Playwright find it.
  const executablePath = process.env.WEETBEATS_CHROMIUM || undefined;
  const browser = await chromium.launch({ executablePath });
  const page = await browser.newPage({ viewport: { width: 1180, height: 760 }, deviceScaleFactor: 2 });

  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("response", (r) => r.status() >= 400 && errors.push(r.status() + " " + r.url()));
  page.on("console", (m) => m.type() === "error" && !m.text().includes("favicon") && errors.push(m.text()));
  page.on("requestfailed", (r) => !r.url().includes("favicon") && errors.push("request failed: " + r.url()));

  await page.addInitScript(SHIM);
  await page.goto(url);
  await page.waitForSelector("#add");

  // --- nothing on screen but the invitation to add something
  check("empty state is showing", await page.locator("#empty").isVisible());
  check("no sample browser", (await page.locator("#samples").count()) === 0);

  // --- the button opens the picker and makes a track from whatever comes back
  await page.evaluate(() => { window.__weetbeats_state.picks = ["/pack/01 kick.wav"]; });
  await page.locator("#addBig").click();
  await page.waitForSelector("#trackHeaders .track");
  check("the big button adds a track", (await page.locator("#trackHeaders .track").count()) === 1);
  check("it went through the picker",
    await page.evaluate(() => window.__weetbeats_calls.some((c) => c.name === "add_instruments")));
  check("empty state goes away", !(await page.locator("#empty").isVisible()));
  check("the track is named after the file",
    (await page.locator("#trackHeaders .track .name").first().textContent()) === "01 kick");

  // --- one trip to the picker can bring back a whole kit
  await page.evaluate(() => {
    window.__weetbeats_state.picks = ["/pack/02 snare.wav", "/pack/04 hat closed.wav"];
  });
  await page.locator("#add").click();
  await page.waitForFunction(() => document.querySelectorAll("#trackHeaders .track").length === 3);
  check("multi-select adds one track each", (await page.locator("#trackHeaders .track").count()) === 3);

  // --- cancelling the picker changes nothing
  await page.evaluate(() => { window.__weetbeats_state.picks = []; });
  await page.locator("#add").click();
  await page.waitForTimeout(150);
  check("cancelling adds nothing", (await page.locator("#trackHeaders .track").count()) === 3);

  // --- a file dropped on the window comes in the same door
  await page.evaluate(() => window.__weetbeats_drop(["/elsewhere/clap.wav"]));
  await page.waitForFunction(() => document.querySelectorAll("#trackHeaders .track").length === 4);
  const dropped = await page.evaluate(() =>
    window.__weetbeats_calls.filter((c) => c.name === "add_dropped").at(-1).args.paths);
  check("dropping a file adds a track", dropped[0] === "/elsewhere/clap.wav", String(dropped));

  // --- dropping something that is not audio says so rather than doing nothing
  await page.evaluate(() => window.__weetbeats_drop(["/elsewhere/notes.txt"]));
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("not a sound file"));
  check("dropping a non-audio file adds nothing",
    (await page.locator("#trackHeaders .track").count()) === 4);
  check("and it says why", (await page.locator("#status").textContent()).includes("notes.txt"),
    await page.locator("#status").textContent());

  // --- back to three rows, so the grid checks below have known dimensions
  await page.locator("#trackHeaders .track").last().locator(".tick.kill").click();
  await page.waitForFunction(() => document.querySelectorAll("#trackHeaders .track").length === 3);

  // --- the grid canvas is sized for the steps and rows
  const size = await page.evaluate(() => {
    const c = document.getElementById("grid");
    return { w: parseInt(c.style.width), h: parseInt(c.style.height) };
  });
  check("grid is 16 steps wide", size.w === 16 * 42, `${size.w}px`);
  check("grid is 3 rows tall", size.h === 3 * 46, `${size.h}px`);

  // --- clicking a step ticks it
  const cell = async (step, row) => {
    const box = await page.locator("#grid").boundingBox();
    return { x: box.x + step * 42 + 21, y: box.y + row * 46 + 23 };
  };
  const c0 = await cell(0, 0);
  await page.mouse.click(c0.x, c0.y);
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_step"));
  const first = await page.evaluate(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_step").at(-1).args);
  check("clicking a box ticks it", first.step === 0 && first.on === true, JSON.stringify(first));

  // --- clicking it again unticks it
  await page.mouse.click(c0.x, c0.y);
  await page.waitForFunction(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_step").length === 2);
  const second = await page.evaluate(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_step").at(-1).args);
  check("clicking it again unticks it", second.on === false, JSON.stringify(second));

  // --- dragging paints a run of boxes on, and never toggles one back off
  await page.evaluate(() => { window.__weetbeats_calls.length = 0; });
  const start = await cell(4, 1);
  const end = await cell(11, 1);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  for (let step = 4; step <= 11; step++) {
    const p = await cell(step, 1);
    await page.mouse.move(p.x, p.y, { steps: 3 });
  }
  await page.mouse.up();
  const painted = await page.evaluate(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_step").map((c) => c.args));
  check("dragging paints a run", painted.length === 8, `${painted.length} boxes`);
  check("painting only turns them on", painted.every((p) => p.on === true));
  check(
    "painted the boxes it was dragged over",
    JSON.stringify(painted.map((p) => p.step)) === JSON.stringify([4, 5, 6, 7, 8, 9, 10, 11]),
    painted.map((p) => p.step).join(","),
  );

  // --- dragging from a ticked box erases instead
  await page.evaluate(() => { window.__weetbeats_calls.length = 0; });
  const s5 = await cell(5, 1);
  await page.mouse.move(s5.x, s5.y);
  await page.mouse.down();
  for (const step of [5, 6, 7]) {
    const p = await cell(step, 1);
    await page.mouse.move(p.x, p.y, { steps: 3 });
  }
  await page.mouse.up();
  const erased = await page.evaluate(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_step").map((c) => c.args));
  check("dragging from a ticked box erases", erased.length === 3 && erased.every((p) => p.on === false),
    JSON.stringify(erased.map((p) => p.on)));

  // --- space plays and stops
  await page.evaluate(() => { window.__weetbeats_calls.length = 0; });
  await page.locator("body").press("Space");
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_playing"));
  const played = await page.evaluate(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_playing").at(-1).args.playing);
  check("space starts it", played === true);
  await page.locator("body").press("Space");
  await page.waitForFunction(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_playing").length === 2);
  const stopped = await page.evaluate(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_playing").at(-1).args.playing);
  check("space stops it", stopped === false);

  // --- space with a slider focused still plays; only text fields should swallow it
  await page.evaluate(() => { window.__weetbeats_calls.length = 0; });
  await page.locator("#bpm").focus();
  await page.keyboard.press("Space");
  check("space works with a slider focused",
    await page.evaluate(() => window.__weetbeats_calls.some((c) => c.name === "set_playing")));
  await page.locator("body").press("Space");

  // --- mute, solo, volume, delete
  await page.evaluate(() => { window.__weetbeats_calls.length = 0; });
  await page.locator("#trackHeaders .track").first().locator(".tick.mute").click();
  await page.locator("#trackHeaders .track").first().locator(".tick.solo").click();
  const flags = await page.evaluate(() => ({
    muted: window.__weetbeats_calls.find((c) => c.name === "set_track_muted")?.args,
    soloed: window.__weetbeats_calls.find((c) => c.name === "set_track_soloed")?.args,
  }));
  check("mute reaches the engine", flags.muted?.muted === true, JSON.stringify(flags.muted));
  check("solo reaches the engine", flags.soloed?.soloed === true, JSON.stringify(flags.soloed));
  check("mute button shows as on",
    await page.locator("#trackHeaders .track").first().locator(".tick.mute").evaluate((n) => n.classList.contains("on")));

  await page.locator("#trackHeaders .track").first().locator("input[type=range]").fill("40");
  const gain = await page.evaluate(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_track_gain").at(-1)?.args);
  check("volume reaches the engine", Math.abs(gain.gain - 0.4) < 1e-6, JSON.stringify(gain));

  // --- bpm
  await page.locator("#bpm").fill("145");
  await page.waitForFunction(() => document.getElementById("bpmValue").textContent === "145");
  check("bpm slider reads back", (await page.locator("#bpmValue").textContent()) === "145");

  // --- the playhead lights the column it is on
  await page.evaluate(() => window.__weetbeats_setStep(6));
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "playhead"));
  await page.waitForTimeout(400);
  check("playhead is tracked", await page.evaluate(() => document.getElementById("play").classList.contains("on")));

  if (process.env.WEETBEATS_SCREENSHOT) {
    await page.screenshot({ path: process.env.WEETBEATS_SCREENSHOT });
  }

  // --- deleting a track
  await page.locator("#trackHeaders .track").first().locator(".tick.kill").click();
  await page.waitForFunction(() => document.querySelectorAll("#trackHeaders .track").length === 2);
  check("deleting a track removes the row", (await page.locator("#trackHeaders .track").count()) === 2);

  check("no page errors", errors.length === 0, JSON.stringify(errors));

  await browser.close();
  server.close();

  const failed = checks.filter((c) => !c.ok);
  console.log(`\n${checks.length - failed.length}/${checks.length} passed`);
  process.exit(failed.length === 0 ? 0 : 1);
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
