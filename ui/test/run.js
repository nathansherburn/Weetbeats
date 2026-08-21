/*
 * Drives the front end in a real browser with the Rust side stubbed out, so the parts
 * that only exist inside a webview — the canvas grids, drag painting, the keyboard, the
 * two views — are actually exercised. Needs Playwright:
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

// Sizes the front end draws with. Must match the constants in main.js.
const CELL = 42;
const ROW = 46;
const LANE = 34;
const SONG_STEP = 4;

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

  const rows = page.locator("#patternList .prow");
  const calls = (name) =>
    page.evaluate((n) => window.__weetbeats_calls.filter((c) => c.name === n), name);
  const lastCall = async (name) => (await calls(name)).at(-1);
  const clearCalls = () => page.evaluate(() => { window.__weetbeats_calls.length = 0; });
  const canvasSize = (id) =>
    page.evaluate((sel) => {
      const c = document.getElementById(sel);
      return { w: parseInt(c.style.width), h: parseInt(c.style.height) };
    }, id);

  // --- a new project starts in its one pattern, not in an empty song
  check("one pattern to start with", (await rows.count()) === 1);
  check("it is named Pattern 1",
    (await rows.first().locator(".pname").textContent()) === "Pattern 1");
  check("it says how long it is", (await rows.first().locator(".plen").textContent()) === "16");
  check("the editor is what you land in", await page.locator("#editor").isVisible());
  check("the song view is not", !(await page.locator("#song").isVisible()));
  check("the patterns panel is there while editing", await page.locator("#patternList").isVisible());
  check("empty state is showing", await page.locator("#empty").isVisible());
  check("no sample browser", (await page.locator("#samples").count()) === 0);
  check("the project has a name", (await page.locator("#projectName").textContent()) === "Untitled");

  // --- the button opens the picker and makes a track from whatever comes back
  await page.evaluate(() => { window.__weetbeats_state.picks = ["/pack/01 kick.wav"]; });
  await page.locator("#addBig").click();
  await page.waitForSelector("#trackHeaders .track");
  check("the big button adds a track", (await page.locator("#trackHeaders .track").count()) === 1);
  check("it went through the picker", (await calls("add_instruments")).length === 1);
  check("empty state goes away", !(await page.locator("#empty").isVisible()));
  check("the track is named after the file",
    (await page.locator("#trackHeaders .track .name").first().textContent()) === "01 kick");
  const kickPath = await page.evaluate(() =>
    [...window.__weetbeats_state.tracks.values()][0].sample.path);
  check("the sample is copied into the project and referred to from there",
    kickPath === "samples/01 kick.wav", kickPath);

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
  const dropped = (await lastCall("add_dropped")).args.paths;
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
  let size = await canvasSize("grid");
  check("grid is 16 steps wide", size.w === 16 * CELL, `${size.w}px`);
  check("grid is 3 rows tall", size.h === 3 * ROW, `${size.h}px`);

  // --- clicking a step ticks it, in the pattern that is open
  const cell = async (step, row) => {
    const box = await page.locator("#grid").boundingBox();
    return { x: box.x + step * CELL + CELL / 2, y: box.y + row * ROW + ROW / 2 };
  };
  const c0 = await cell(0, 0);
  await page.mouse.click(c0.x, c0.y);
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_step"));
  const first = (await lastCall("set_step")).args;
  check("clicking a box ticks it",
    first.step === 0 && first.on === true && first.pattern === 0, JSON.stringify(first));

  // --- clicking it again unticks it
  await page.mouse.click(c0.x, c0.y);
  await page.waitForFunction(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_step").length === 2);
  check("clicking it again unticks it", (await lastCall("set_step")).args.on === false);

  // --- dragging paints a run of boxes on, and never toggles one back off
  await clearCalls();
  const start = await cell(4, 1);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  for (let step = 4; step <= 11; step++) {
    const p = await cell(step, 1);
    await page.mouse.move(p.x, p.y, { steps: 3 });
  }
  await page.mouse.up();
  const painted = (await calls("set_step")).map((c) => c.args);
  check("dragging paints a run", painted.length === 8, `${painted.length} boxes`);
  check("painting only turns them on", painted.every((p) => p.on === true));
  check(
    "painted the boxes it was dragged over",
    JSON.stringify(painted.map((p) => p.step)) === JSON.stringify([4, 5, 6, 7, 8, 9, 10, 11]),
    painted.map((p) => p.step).join(","),
  );

  // --- dragging from a ticked box erases instead
  await clearCalls();
  const s5 = await cell(5, 1);
  await page.mouse.move(s5.x, s5.y);
  await page.mouse.down();
  for (const step of [5, 6, 7]) {
    const p = await cell(step, 1);
    await page.mouse.move(p.x, p.y, { steps: 3 });
  }
  await page.mouse.up();
  const erased = (await calls("set_step")).map((c) => c.args);
  check("dragging from a ticked box erases", erased.length === 3 && erased.every((p) => !p.on),
    JSON.stringify(erased.map((p) => p.on)));

  if (process.env.WEETBEATS_SCREENSHOT) {
    await page.screenshot({ path: process.env.WEETBEATS_SCREENSHOT });
  }

  // --- how many boxes a pattern has
  await page.locator("#moreSteps").click();
  await page.waitForFunction(() => document.getElementById("steps").value === "20");
  size = await canvasSize("grid");
  check("more steps makes a wider grid", size.w === 20 * CELL, `${size.w}px`);
  check("and the panel says so", (await rows.first().locator(".plen").textContent()) === "20");

  await page.locator("#fewerSteps").click();
  await page.waitForFunction(() => document.getElementById("steps").value === "16");
  check("fewer steps takes it back", (await canvasSize("grid")).w === 16 * CELL);

  // --- shortening a pattern drops the notes that fall off the end
  await page.mouse.click((await cell(12, 0)).x, (await cell(12, 0)).y);
  await page.waitForFunction(() =>
    window.__weetbeats_state.patterns[0].lanes.some((l) => l.notes.some((n) => n.step === 12)));
  await page.locator("#steps").fill("8");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "8");
  const trimmed = await page.evaluate(() =>
    window.__weetbeats_state.patterns[0].lanes.flatMap((l) => l.notes.map((n) => n.step)));
  check("shortening drops the notes off the end", trimmed.every((step) => step < 8),
    trimmed.join(","));
  check("and the grid gets shorter with it", (await canvasSize("grid")).w === 8 * CELL);
  // Boxes past the end are not there to click any more.
  await clearCalls();
  const box = await page.locator("#grid").boundingBox();
  await page.mouse.click(box.x + 12 * CELL, box.y + ROW / 2);
  await page.waitForTimeout(120);
  check("and there is nothing past the end to click", (await calls("set_step")).length === 0);
  await page.locator("#steps").fill("16");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "16");

  // --- clicking the open pattern closes it, and the panel stays put
  await rows.first().click();
  await page.waitForSelector("#song:visible");
  check("clicking the open pattern goes back to the song", await page.locator("#song").isVisible());
  check("and the editor goes away", !(await page.locator("#editor").isVisible()));
  check("the panel is still there in the song view", await page.locator("#patternList").isVisible());
  check("Rust was told the pattern closed", (await calls("close_pattern")).length === 1);
  check("the song view says what to do", await page.locator("#songHint").isVisible());

  // --- and clicking it again opens it
  await rows.first().click();
  await page.waitForSelector("#editor:visible");
  check("clicking it again opens it", await page.locator("#editor").isVisible());
  check("Rust was told which pattern", (await lastCall("open_pattern")).args.id === 0);

  // --- escape closes the pattern too
  await page.locator("body").press("Escape");
  await page.waitForSelector("#song:visible");
  check("escape closes the pattern", await page.locator("#song").isVisible());

  // --- escape in the song view is still the panic button
  await clearCalls();
  await page.locator("body").press("Escape");
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "panic_stop"));
  check("escape in the song view stops everything", (await calls("panic_stop")).length === 1);

  // --- a new pattern, and it opens
  await page.locator("#addPattern").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 2);
  check("add makes a pattern", (await rows.count()) === 2);
  check("it is named after the next free number",
    (await rows.nth(1).locator(".pname").textContent()) === "Pattern 2");
  check("and it opens, because it is empty", await page.locator("#editor").isVisible());
  check("the new pattern is the open one",
    await rows.nth(1).evaluate((n) => n.classList.contains("open")));
  check("its grid starts empty", await page.evaluate(() =>
    window.__weetbeats_state.patterns[1].lanes.length === 0));

  // --- double click a name to change it
  await rows.nth(1).dblclick();
  await page.waitForSelector("#patternList .rename");
  check("double click starts a rename", (await page.locator("#patternList .rename").count()) === 1);
  await page.locator("#patternList .rename").fill("Chorus");
  await page.locator("#patternList .rename").press("Enter");
  await page.waitForFunction(() =>
    document.querySelectorAll("#patternList .prow")[1].textContent.includes("Chorus"));
  check("the new name sticks", (await rows.nth(1).locator(".pname").textContent()) === "Chorus");
  check("and Rust has it", (await lastCall("rename_pattern")).args.name === "Chorus");

  // --- escape out of a rename keeps the old name, and does not close the pattern
  await rows.nth(1).dblclick();
  await page.waitForSelector("#patternList .rename");
  await page.locator("#patternList .rename").fill("Nope");
  await page.locator("#patternList .rename").press("Escape");
  await page.waitForFunction(() => !document.querySelector("#patternList .rename"));
  check("escape drops the rename", (await rows.nth(1).locator(".pname").textContent()) === "Chorus");
  check("and stays in the pattern", await page.locator("#editor").isVisible());

  // --- duplicating brings the notes with it
  await rows.first().click(); // open Pattern 1, which has notes in it
  await page.waitForSelector("#editor:visible");
  await rows.first().hover();
  await rows.first().locator(".tick.dup").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 3);
  check("duplicate makes a third pattern", (await rows.count()) === 3);
  check("the copy lands next to the original", await page.evaluate(() => {
    const s = window.__weetbeats_state;
    // Named for the lowest free number, which is 2 again now that one is called Chorus.
    return s.patterns[0].id === 0 && s.patterns[1].name === "Pattern 2";
  }));
  check("the copy has the same notes", await page.evaluate(() => {
    const s = window.__weetbeats_state;
    const steps = (p) => p.lanes.flatMap((l) => l.notes.map((n) => n.step)).sort().join(",");
    return steps(s.patterns[0]) === steps(s.patterns[1]) && steps(s.patterns[0]).length > 0;
  }));
  check("and the copy is what you are editing",
    await rows.nth(1).evaluate((n) => n.classList.contains("open")));

  // --- deleting a pattern
  await rows.nth(1).hover();
  await rows.nth(1).locator(".tick.kill").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 2);
  check("delete removes the pattern", (await rows.count()) === 2);
  check("deleting the open one goes back to the song", await page.locator("#song").isVisible());

  // --- the song: one slot is one whole pattern
  const laneCell = async (slot, row) => {
    const box = await page.locator("#lanes").boundingBox();
    return { x: box.x + slot + 6, y: box.y + row * LANE + LANE / 2 };
  };
  let song = await canvasSize("lanes");
  check("an empty song is one spare slot wide", song.w === 16 * SONG_STEP, `${song.w}px`);
  check("the song has a lane per pattern", song.h === 2 * LANE, `${song.h}px`);

  const firstSlot = await laneCell(0, 0);
  await page.mouse.click(firstSlot.x, firstSlot.y);
  await page.waitForFunction(() => window.__weetbeats_state.song.length === 1);
  check("ticking a slot puts the pattern in the song", await page.evaluate(() =>
    window.__weetbeats_state.song[0] === 0));
  check("the slot Rust was told is the first one", (await lastCall("set_song_slot")).args.index === 0);
  check("the hint goes away", !(await page.locator("#songHint").isVisible()));
  song = await canvasSize("lanes");
  check("the song is now a 16 step block plus the spare",
    song.w === 16 * SONG_STEP + 16 * SONG_STEP, `${song.w}px`);

  // --- a longer pattern is a longer block
  await rows.nth(1).click();
  await page.waitForSelector("#editor:visible");
  await page.locator("#steps").fill("32");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "32");
  // Escape works from the steps box too: it is a number field, it has no use for it.
  await page.keyboard.press("Escape");
  await page.waitForSelector("#song:visible");
  const second = await laneCell(16 * SONG_STEP, 1);
  await page.mouse.click(second.x, second.y);
  await page.waitForFunction(() => window.__weetbeats_state.song.length === 2);
  song = await canvasSize("lanes");
  check("a 32 step pattern is twice the block",
    song.w === (16 + 32 + 16) * SONG_STEP, `${song.w}px`);

  if (process.env.WEETBEATS_SCREENSHOT) {
    await page.screenshot({ path: process.env.WEETBEATS_SCREENSHOT.replace(/\.png$/, "-song.png") });
  }

  // --- the panel keeps up with the playhead without being rebuilt under your hands
  await page.evaluate(() => {
    document.querySelectorAll("#patternList .prow")[0].dataset.marked = "yes";
    window.__weetbeats_setStep(2, 0);
  });
  await page.waitForFunction(() =>
    document.querySelectorAll("#patternList .prow")[0].classList.contains("playing"));
  await page.evaluate(() => window.__weetbeats_setStep(2, 1));
  await page.waitForFunction(() =>
    document.querySelectorAll("#patternList .prow")[1].classList.contains("playing"));
  check("the playing mark follows the song from slot to slot",
    !(await rows.first().evaluate((n) => n.classList.contains("playing"))));
  check("and the rows are not rebuilt while it plays, so a rename survives",
    (await rows.first().evaluate((n) => n.dataset.marked)) === "yes");

  // --- ticking a slot that is already that pattern takes it out again
  await clearCalls();
  const again = await laneCell(0, 0);
  await page.mouse.click(again.x, again.y);
  await page.waitForFunction(() => window.__weetbeats_state.song.length === 1);
  check("ticking a filled slot clears it", (await calls("clear_song_slot")).length === 1);
  check("and the song closes the gap", await page.evaluate(() =>
    window.__weetbeats_state.song.length === 1 && window.__weetbeats_state.song[0] !== 0));

  // --- the scrubber plays from a slot
  await clearCalls();
  const scrubber = await page.locator("#scrubber").boundingBox();
  await page.mouse.click(scrubber.x + 4, scrubber.y + scrubber.height / 2);
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "seek_song"));
  check("clicking the scrubber seeks", (await lastCall("seek_song")).args.index === 0);

  // --- the playhead is followed, and says which pattern is sounding
  await page.evaluate(() => window.__weetbeats_setStep(4, 0));
  await page.waitForFunction(() =>
    document.querySelector("#patternList .prow.playing") !== null, null, { timeout: 4000 });
  check("the panel shows which pattern is playing",
    await rows.nth(1).evaluate((n) => n.classList.contains("playing")));
  check("play shows as on", await page.evaluate(() =>
    document.getElementById("play").classList.contains("on")));

  // --- space plays and stops
  await clearCalls();
  await page.locator("body").press("Space");
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_playing"));
  check("space stops it", (await lastCall("set_playing")).args.playing === false);
  await page.locator("body").press("Space");
  await page.waitForFunction(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_playing").length === 2);
  check("space starts it again", (await lastCall("set_playing")).args.playing === true);
  await page.locator("body").press("Space");

  // --- space with a slider focused still plays; only text fields should swallow it
  await clearCalls();
  await page.locator("#bpm").focus();
  await page.keyboard.press("Space");
  check("space works with a slider focused", (await calls("set_playing")).length > 0);

  // --- mute, solo, volume, delete
  await rows.first().click();
  await page.waitForSelector("#editor:visible");
  await clearCalls();
  await page.locator("#trackHeaders .track").first().locator(".tick.mute").click();
  await page.locator("#trackHeaders .track").first().locator(".tick.solo").click();
  check("mute reaches the engine", (await lastCall("set_track_muted")).args.muted === true);
  check("solo reaches the engine", (await lastCall("set_track_soloed")).args.soloed === true);
  check("mute button shows as on", await page.locator("#trackHeaders .track").first()
    .locator(".tick.mute").evaluate((n) => n.classList.contains("on")));

  await page.locator("#trackHeaders .track").first().locator("input[type=range]").fill("40");
  const gain = (await lastCall("set_track_gain")).args;
  check("volume reaches the engine", Math.abs(gain.gain - 0.4) < 1e-6, JSON.stringify(gain));

  // --- bpm
  await page.locator("#bpm").fill("145");
  await page.waitForFunction(() => document.getElementById("bpmValue").textContent === "145");
  check("bpm slider reads back", (await page.locator("#bpmValue").textContent()) === "145");

  // --- deleting a track takes its notes out of every pattern
  await page.locator("#trackHeaders .track").first().locator(".tick.kill").click();
  await page.waitForFunction(() => document.querySelectorAll("#trackHeaders .track").length === 2);
  check("deleting a track removes the row", (await page.locator("#trackHeaders .track").count()) === 2);
  check("and its notes go with it", await page.evaluate(() =>
    window.__weetbeats_state.patterns.every((p) => p.lanes.every((l) => l.track !== 0))));

  // --- saving
  await clearCalls();
  await page.keyboard.press("Meta+s");
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "save_project"));
  check("cmd S writes the project out", (await calls("save_project")).length === 1);
  check("and it says so", (await page.locator("#status").textContent()).includes("saved"),
    await page.locator("#status").textContent());

  await page.locator("#saveAs").click();
  await page.waitForFunction(() => document.getElementById("projectName").textContent === "Newer");
  check("save as renames the project", (await page.locator("#projectName").textContent()) === "Newer");

  // --- cancelling the open dialog leaves everything alone
  await page.locator("#openProject").click();
  await page.waitForTimeout(150);
  check("cancelling open changes nothing",
    (await page.locator("#projectName").textContent()) === "Newer" && (await rows.count()) === 2);

  // --- and the last pattern cannot be deleted
  await rows.nth(1).hover();
  await rows.nth(1).locator(".tick.kill").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 1);
  await rows.first().hover();
  await rows.first().locator(".tick.kill").click();
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("at least one"));
  check("the last pattern stays", (await rows.count()) === 1);
  check("and it says why", (await page.locator("#status").textContent()).includes("at least one"),
    await page.locator("#status").textContent());

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
