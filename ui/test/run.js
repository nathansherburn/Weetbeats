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
  await page.waitForFunction(() => document.querySelectorAll("#samples li").length > 0);

  // --- the browser lists what Rust found
  check("sample list renders", (await page.locator("#samples li").count()) === 8);
  check("empty state is showing", await page.locator("#empty").isVisible());

  // --- search filters it
  await page.fill("#filter", "hat");
  check("search filters the list", (await page.locator("#samples li").count()) === 2);
  await page.fill("#filter", "");

  // --- clicking previews
  await page.locator("#samples li", { hasText: "01 kick" }).click();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "preview"));
  const preview = await page.evaluate(() =>
    window.__weetbeats_calls.find((c) => c.name === "preview").args.path);
  check("clicking a sample previews it", preview === "/pack/01 kick.wav", preview);

  // --- dragging one in makes a track (drag and drop, driven by hand)
  const drop = async (name) => {
    await page.evaluate(async (sampleName) => {
      const li = [...document.querySelectorAll("#samples li")]
        .find((n) => n.textContent.includes(sampleName));
      const dt = new DataTransfer();
      li.dispatchEvent(new DragEvent("dragstart", { dataTransfer: dt, bubbles: true }));
      const pattern = document.getElementById("pattern");
      pattern.dispatchEvent(new DragEvent("dragover", { dataTransfer: dt, bubbles: true, cancelable: true }));
      pattern.dispatchEvent(new DragEvent("drop", { dataTransfer: dt, bubbles: true, cancelable: true }));
    }, name);
    await page.waitForFunction(
      (n) => document.querySelectorAll("#trackHeaders .track").length === n,
      undefined,
      { timeout: 3000 },
    ).catch(() => {});
  };

  await drop("01 kick");
  await page.waitForSelector("#trackHeaders .track");
  check("dropping a sample adds a track", (await page.locator("#trackHeaders .track").count()) === 1);
  check("empty state goes away", !(await page.locator("#empty").isVisible()));

  await drop("02 snare");
  await drop("04 hat closed");
  check("three tracks", (await page.locator("#trackHeaders .track").count()) === 3);

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

  // --- space in the search box types a space instead of playing
  await page.evaluate(() => { window.__weetbeats_calls.length = 0; });
  await page.click("#filter");
  await page.keyboard.press("Space");
  const playedWhileTyping = await page.evaluate(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_playing"));
  check("space while searching does not play", !playedWhileTyping);
  await page.fill("#filter", "");

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
