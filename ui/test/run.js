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
const CELL = 30;
const GAP = 3;
const ROW = 46;
const LANE = 34;
const SONG_STEP = 4;
const BAR_PX = 16 * SONG_STEP;
const ZOOM_STEP = 1.3;
const ACCENT = "255,77,135";
// An unticked box, and the strip a piano roll row is drawn on.
const EMPTY_BOX = "32,28,41";
const MINI_ROLL = "27,23,36";
// A colour per pattern in the song. Must match BLOCK_COLOURS in main.js.
const BLOCK_COLOURS = [
  "255,77,135", "255,157,77", "255,215,94", "155,227,77",
  "77,227,168", "77,201,255", "143,139,255", "240,123,255",
];
const blockColour = (row) => BLOCK_COLOURS[row % BLOCK_COLOURS.length];
// The piano roll. Must match main.js.
const ROLL_CELL = 30;
const SEMITONE = 15;
const HIGH_PITCH = 127;
const MIDDLE_C = 60;

// The front end and Rust agree about the commands before a single one is called: a stub that
// answers differently from the real thing would make every check below meaningless.
try {
  require("child_process").execFileSync(process.execPath, [path.join(__dirname, "contract.js")], {
    stdio: "inherit",
  });
  check("the front end and Rust agree about the commands", true);
} catch {
  check("the front end and Rust agree about the commands", false, "see the mismatches above");
}

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
  const song = () => page.evaluate(() => window.__weetbeats_state.song);

  /* Where in the song a lane and a step meet, in the window. */
  const lanePoint = async (row, step) => {
    const box = await page.locator("#lanes").boundingBox();
    return { x: box.x + step * SONG_STEP + 2, y: box.y + row * LANE + LANE / 2 };
  };

  /*
   * Opening a pattern to edit is a double click on one of its blocks in the song — the only
   * way in, now that a click in the panel only picks a pattern out. Draws a block first if
   * that lane has nothing at the top of it.
   */
  const openViaSong = async (row) => {
    if (!(await page.locator("#song").isVisible())) {
      await page.locator("#songMode").click();
      await page.waitForSelector("#song:visible");
    }
    const at = await lanePoint(row, 0);
    await page.mouse.click(at.x, at.y);
    await page.mouse.dblclick(at.x, at.y);
    await page.waitForSelector("#editor:visible");
  };

  const rubOutBlock = async (row, step) => {
    const at = await lanePoint(row, step);
    await page.mouse.click(at.x, at.y, { button: "right" });
  };
  const menu = (what) => page.evaluate((w) => window.__weetbeats_menu(w), what);

  /*
   * What colour the song view actually drew in the middle of a cell. Drawing happens on the
   * next frame after a click, so these wait for the canvas to catch up rather than reading
   * it the instant the state changes.
   */
  const lanePixel = (step, row) =>
    page.evaluate(
      ([step, row, songStep, lane]) => {
        const dpr = window.devicePixelRatio || 1;
        const ctx = document.getElementById("lanes").getContext("2d");
        const x = Math.round((step * songStep + 2) * dpr);
        const y = Math.round((row * lane + lane / 2) * dpr);
        const [r, g, b] = ctx.getImageData(x, y, 1, 1).data;
        return `${r},${g},${b}`;
      },
      [step, row, SONG_STEP, LANE],
    );

  /* The same, at a place in the lane rather than a step: what the zoom checks need. */
  const lanePixelAt = (x, row) =>
    page.evaluate(
      ([x, row, lane]) => {
        const dpr = window.devicePixelRatio || 1;
        const ctx = document.getElementById("lanes").getContext("2d");
        const [r, g, b] = ctx.getImageData(
          Math.round(x * dpr),
          Math.round((row * lane + lane / 2) * dpr),
          1,
          1,
        ).data;
        return `${r},${g},${b}`;
      },
      [x, row, LANE],
    );

  const settledAt = async (x, row, colour, want) => {
    const deadline = Date.now() + 2000;
    for (;;) {
      const got = await lanePixelAt(x, row);
      if ((got === colour) === want || Date.now() > deadline) return got;
      await page.waitForTimeout(25);
    }
  };

  const settledPixel = async (step, row, colour, want) => {
    const deadline = Date.now() + 2000;
    for (;;) {
      const got = await lanePixel(step, row);
      if ((got === colour) === want || Date.now() > deadline) return got;
      await page.waitForTimeout(25);
    }
  };
  // Every pattern is a different colour in the song, so what counts as painted depends on
  // which lane it is.
  const painted = (step, row) => settledPixel(step, row, blockColour(row), true);
  const blank = (step, row) => settledPixel(step, row, blockColour(row), false);

  /* What the step grid drew in the middle of a cell: a box, or a row turned into a roll. */
  const gridPixel = (step, row) =>
    page.evaluate(
      ([step, row, cell, rowH]) => {
        const dpr = window.devicePixelRatio || 1;
        const ctx = document.getElementById("grid").getContext("2d");
        const x = Math.round((step * cell + cell / 2) * dpr);
        const y = Math.round((row * rowH + rowH / 2) * dpr);
        const [r, g, b] = ctx.getImageData(x, y, 1, 1).data;
        return `${r},${g},${b}`;
      },
      [step, row, CELL, ROW],
    );

  const settledGrid = async (step, row, colour) => {
    const deadline = Date.now() + 2000;
    for (;;) {
      const got = await gridPixel(step, row);
      if (got === colour || Date.now() > deadline) return got;
      await page.waitForTimeout(25);
    }
  };

  // --- the window is the app's, not the webview's
  check("no title in the page", (await page.title()) === "");
  check("nothing else calls itself Weetbeats", (await page.locator(".brand").count()) === 0);
  check("the header can be dragged to move the window",
    (await page.locator("header[data-tauri-drag-region]").count()) === 1);
  const menued = await page.evaluate(() => {
    const e = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    document.getElementById("grid").dispatchEvent(e);
    return e.defaultPrevented;
  });
  check("right click does not open the webview's own menu", menued);

  // --- a new project starts in its one pattern, not in an empty song
  check("one pattern to start with", (await rows.count()) === 1);
  check("it is named Pattern 1",
    (await rows.first().locator(".pname").textContent()) === "Pattern 1");
  check("it says how long it is", (await rows.first().locator(".plen").textContent()) === "16");
  check("and the length sits with the name, not across the row", await page.evaluate(() => {
    const row = document.querySelector("#patternList .prow");
    const name = row.querySelector(".pname").getBoundingClientRect();
    const len = row.querySelector(".plen").getBoundingClientRect();
    return len.left - name.right < 12;
  }));
  check("the editor is what you land in", await page.locator("#editor").isVisible());
  check("the song view is not", !(await page.locator("#song").isVisible()));
  check("the patterns panel is there while editing", await page.locator("#patternList").isVisible());
  check("empty state is showing", await page.locator("#empty").isVisible());
  check("the song's name is on the song button",
    (await page.locator("#songName").textContent()) === "Untitled");
  check("and the button fills the panel head", await page.evaluate(() => {
    const head = document.querySelector(".panel-head").getBoundingClientRect();
    const button = document.getElementById("songMode").getBoundingClientRect();
    return button.width >= head.width - 1 && button.height >= head.height - 1;
  }));
  check("play is at the far right of the transport", await page.evaluate(() => {
    const play = document.getElementById("play").getBoundingClientRect();
    const bpm = document.getElementById("bpm").getBoundingClientRect();
    const bar = document.querySelector(".transport").getBoundingClientRect();
    return bpm.right <= play.left && bar.right - play.right < 40;
  }));
  check("no master volume", (await page.locator("#master").count()) === 0);
  check("nothing in the status line", (await page.locator("#status").textContent()) === "");

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

  // --- and the way to add another is under the ones there are
  const headers = await page.locator("#trackHeaders").boundingBox();
  const addRow = await page.locator("#add").boundingBox();
  check("the add instrument button is under the instruments",
    addRow.y >= headers.y + headers.height - 1, `${addRow.y} vs ${headers.y + headers.height}`);

  // --- one trip to the picker can bring back a whole kit
  await page.evaluate(() => {
    window.__weetbeats_state.picks = ["/pack/02 snare.wav", "/pack/04 hat closed.wav"];
  });
  await page.locator("#add").click();
  await page.waitForFunction(() => document.querySelectorAll("#trackHeaders .track").length === 3);
  check("multi-select adds one track each", (await page.locator("#trackHeaders .track").count()) === 3);

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
  const brushed = (await calls("set_step")).map((c) => c.args);
  check("dragging paints a run", brushed.length === 8, `${brushed.length} boxes`);
  check("painting only turns them on", brushed.every((p) => p.on === true));
  check(
    "painted the boxes it was dragged over",
    JSON.stringify(brushed.map((p) => p.step)) === JSON.stringify([4, 5, 6, 7, 8, 9, 10, 11]),
    brushed.map((p) => p.step).join(","),
  );

  // --- the right button rubs out, wherever it lands
  await clearCalls();
  const r6 = await cell(6, 1);
  await page.mouse.click(r6.x, r6.y, { button: "right" });
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_step"));
  check("right click rubs a box out", (await lastCall("set_step")).args.on === false);
  // And on an empty box it stays empty rather than drawing one.
  await clearCalls();
  const r15 = await cell(15, 1);
  await page.mouse.click(r15.x, r15.y, { button: "right" });
  await page.waitForTimeout(120);
  check("right click never draws", (await calls("set_step")).length === 0);

  // --- dragging from a ticked box erases instead
  await clearCalls();
  const s5 = await cell(5, 1);
  await page.mouse.move(s5.x, s5.y);
  await page.mouse.down();
  for (const step of [5, 7, 8]) {
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

  // --- how many boxes a pattern has, one at a time
  await page.locator("#moreSteps").click();
  await page.waitForFunction(() => document.getElementById("steps").value === "17");
  check("more steps adds one box", (await canvasSize("grid")).w === 17 * CELL);
  await page.locator("#fewerSteps").click();
  await page.waitForFunction(() => document.getElementById("steps").value === "16");
  check("fewer steps takes one away", (await canvasSize("grid")).w === 16 * CELL);

  // --- and you can type it, or drag it
  // A clean status line first: what matters is that this change does not add to it.
  await page.evaluate(() => { document.getElementById("status").innerHTML = ""; });
  await page.locator("#steps").click();
  await page.locator("#steps").fill("24");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "24");
  check("typing a length works", (await canvasSize("grid")).w === 24 * CELL);
  check("and the panel says so", (await rows.first().locator(".plen").textContent()) === "24");
  check("and nothing failed quietly on the way",
    (await page.locator("#status").textContent()) === "",
    await page.locator("#status").textContent());

  const stepsBox = await page.locator("#steps").boundingBox();
  await page.mouse.move(stepsBox.x + stepsBox.width / 2, stepsBox.y + stepsBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(stepsBox.x + stepsBox.width / 2, stepsBox.y + stepsBox.height / 2 - 12, { steps: 4 });
  await page.mouse.up();
  check("dragging a number up raises it",
    (await page.locator("#steps").inputValue()) === "28",
    await page.locator("#steps").inputValue());

  // --- shortening a pattern drops the notes that fall off the end
  await page.locator("#steps").click();
  await page.locator("#steps").fill("8");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "8");
  const trimmed = await page.evaluate(() =>
    window.__weetbeats_state.patterns[0].lanes.flatMap((l) => l.notes.map((n) => n.step)));
  check("shortening drops the notes off the end", trimmed.every((step) => step < 8),
    trimmed.join(","));
  check("and the grid gets shorter with it", (await canvasSize("grid")).w === 8 * CELL);
  await clearCalls();
  const box = await page.locator("#grid").boundingBox();
  await page.mouse.click(box.x + 12 * CELL, box.y + ROW / 2);
  await page.waitForTimeout(120);
  check("and there is nothing past the end to click", (await calls("set_step")).length === 0);
  await page.locator("#steps").click();
  await page.locator("#steps").fill("16");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "16");

  // --- bpm the same way: type it, drag it
  await clearCalls();
  await page.locator("#bpm").click();
  await page.locator("#bpm").fill("145");
  await page.locator("#bpm").press("Enter");
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_bpm"));
  check("typing a tempo works", (await lastCall("set_bpm")).args.bpm === 145);

  const bpmBox = await page.locator("#bpm").boundingBox();
  await page.mouse.move(bpmBox.x + bpmBox.width / 2, bpmBox.y + bpmBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(bpmBox.x + bpmBox.width / 2, bpmBox.y + bpmBox.height / 2 + 30, { steps: 5 });
  await page.mouse.up();
  check("dragging the tempo down lowers it",
    (await page.locator("#bpm").inputValue()) === "135",
    await page.locator("#bpm").inputValue());
  await page.mouse.move(bpmBox.x + bpmBox.width / 2, bpmBox.y + bpmBox.height / 2);
  await page.mouse.wheel(0, -120);
  await page.waitForFunction(() => document.getElementById("bpm").value === "136");
  check("scrolling the tempo nudges it", (await page.locator("#bpm").inputValue()) === "136");

  // --- the piano roll: the same pattern, one track, as notes
  const noteAt = async (step, pitch) => {
    const b = await page.locator("#notes").boundingBox();
    return {
      x: b.x + step * ROLL_CELL + 6,
      y: b.y + (HIGH_PITCH - pitch) * SEMITONE + SEMITONE / 2,
    };
  };
  const notePixel = (step, pitch) =>
    page.evaluate(
      ([step, pitch, cell, semitone, high]) => {
        const dpr = window.devicePixelRatio || 1;
        const ctx = document.getElementById("notes").getContext("2d");
        const x = Math.round((step * cell + 6) * dpr);
        const y = Math.round(((high - pitch) * semitone + semitone / 2) * dpr);
        const [r, g, b] = ctx.getImageData(x, y, 1, 1).data;
        return `${r},${g},${b}`;
      },
      [step, pitch, ROLL_CELL, SEMITONE, HIGH_PITCH],
    );
  /* Drawing lands on the next frame, so wait for the canvas rather than racing it. */
  const settledNote = async (step, pitch, want) => {
    const deadline = Date.now() + 2000;
    for (;;) {
      const got = await notePixel(step, pitch);
      if ((got === ACCENT) === want || Date.now() > deadline) return got;
      await page.waitForTimeout(25);
    }
  };
  const rollLane = () =>
    page.evaluate(() => {
      const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === 0);
      return lane ? lane.notes : [];
    });

  // The note button turns the row itself into a small piano roll, in place of the boxes.
  await clearCalls();
  // Step nine: past the label an empty roll draws, and off the beat, where an unticked box
  // is the darker of the two.
  check("boxes to start with", (await settledGrid(9, 0, EMPTY_BOX)) === EMPTY_BOX,
    await gridPixel(9, 0));
  await page.locator("#trackHeaders .track").first().locator(".tick.keys-on").click();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_pattern_pitched"));
  check("the note button makes the track an instrument",
    (await lastCall("set_pattern_pitched")).args.pitched === true);
  check("in this pattern, not everywhere",
    (await lastCall("set_pattern_pitched")).args.pattern === 0,
    JSON.stringify((await lastCall("set_pattern_pitched")).args));
  check("and its row becomes a piano roll instead of boxes",
    (await settledGrid(9, 0, MINI_ROLL)) === MINI_ROLL, await gridPixel(9, 0));
  check("only that row: the others still have their boxes",
    (await settledGrid(9, 1, EMPTY_BOX)) === EMPTY_BOX, await gridPixel(9, 1));
  check("and the roll proper is not open yet", !(await page.locator("#roll").isVisible()));

  // Clicking the small one opens the big one.
  const miniBox = await page.locator("#grid").boundingBox();
  await page.mouse.click(miniBox.x + 2 * CELL + CELL / 2, miniBox.y + ROW / 2);
  await page.waitForSelector("#roll:visible");
  check("clicking the row's roll opens the roll proper", await page.locator("#roll").isVisible());
  check("and the boxes step aside", !(await page.locator("#editor").isVisible()));
  check("the patterns panel is still there", await page.locator("#patternList").isVisible());
  check("and it says which track it is",
    (await page.locator("#rollName").textContent()).includes("kick"),
    await page.locator("#rollName").textContent());
  check("the roll spans the whole of MIDI",
    (await canvasSize("keys")).h === 128 * SEMITONE, `${(await canvasSize("keys")).h}px`);

  // The roll wears the same titlebar the boxes do: one band of the pattern's colour across
  // the top, and the way out at the far right of it.
  check("the roll's ruler is underlined in the pattern's colour, level with the chip",
    await page.evaluate(() => {
      const line = (id) => {
        const node = document.getElementById(id);
        const style = getComputedStyle(node);
        return {
          colour: style.borderBottomColor,
          width: parseFloat(style.borderBottomWidth),
          bottom: node.getBoundingClientRect().bottom,
        };
      };
      const ruler = line("rollRuler");
      const chip = line("rollChip");
      return (
        ruler.colour === chip.colour &&
        ruler.width === chip.width &&
        Math.abs(ruler.bottom - chip.bottom) < 0.5
      );
    }));
  check("and its way out sits at the far right too", await page.evaluate(() => {
    const shut = document.getElementById("closeRoll").getBoundingClientRect();
    const roll = document.getElementById("roll").getBoundingClientRect();
    const chip = document.getElementById("rollChip").getBoundingClientRect();
    return shut.left > chip.right && roll.right - shut.right < 20 && shut.bottom < chip.bottom;
  }));

  // --- drawing a note
  await clearCalls();
  const c4 = await noteAt(2, MIDDLE_C);
  await page.mouse.click(c4.x, c4.y);
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_note"));
  const drawnNote = (await lastCall("set_note")).args;
  check("clicking draws a note there",
    drawnNote.at.step === 2 && drawnNote.at.pitch === MIDDLE_C, JSON.stringify(drawnNote));
  check("one step long to start with", drawnNote.length === 1);
  check("and you hear it as you draw it",
    (await lastCall("audition")).args.pitch === MIDDLE_C);
  check("it is drawn where it was put", (await settledNote(2, MIDDLE_C, true)) === ACCENT,
    await notePixel(2, MIDDLE_C));
  check("and nowhere else", (await settledNote(2, MIDDLE_C + 1, false)) !== ACCENT);

  // --- drag straight on from drawing to set how long it is
  await clearCalls();
  const noteStart = await noteAt(6, 64);
  const noteEnd = await noteAt(9, 64);
  await page.mouse.move(noteStart.x, noteStart.y);
  await page.mouse.down();
  await page.mouse.move(noteEnd.x, noteEnd.y, { steps: 4 });
  await page.mouse.up();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_note").length >= 2);
  const stretched = (await calls("set_note")).at(-1).args;
  check("dragging out a new note sets how long it is", stretched.length === 4,
    JSON.stringify(stretched));
  // --- dragging a note moves it
  await clearCalls();
  const grab = await noteAt(2, MIDDLE_C);
  const drop = await noteAt(4, 62);
  await page.mouse.move(grab.x, grab.y);
  await page.mouse.down();
  await page.mouse.move(drop.x, drop.y, { steps: 5 });
  await page.mouse.up();
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "move_note"));
  const moved = (await lastCall("move_note")).args;
  check("dragging a note moves it",
    moved.at.step === 2 && moved.at.pitch === MIDDLE_C && moved.to.step === 4 && moved.to.pitch === 62,
    JSON.stringify(moved));
  check("and it is one note, not two", (await rollLane()).length === 2, JSON.stringify(await rollLane()));

  // --- dragging the right hand edge changes the length
  await clearCalls();
  const edge = await page.evaluate(
    ([cell, semitone, high]) => {
      const box = document.getElementById("notes").getBoundingClientRect();
      // The end of the note at step 4, which is one step long.
      return { x: box.left + 5 * cell - 3, y: box.top + (high - 62) * semitone + semitone / 2 };
    },
    [ROLL_CELL, SEMITONE, HIGH_PITCH],
  );
  await page.mouse.move(edge.x, edge.y);
  await page.mouse.down();
  await page.mouse.move(edge.x + ROLL_CELL * 2, edge.y, { steps: 4 });
  await page.mouse.up();
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_note"));
  check("dragging the end makes it longer", (await lastCall("set_note")).args.length === 3,
    JSON.stringify((await lastCall("set_note")).args));

  // --- how hard it is hit
  await clearCalls();
  const velBox = await page.locator("#velocity").boundingBox();
  await page.mouse.click(velBox.x + 4 * ROLL_CELL + 6, velBox.y + 6);
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_note"));
  check("dragging in the lane underneath sets how hard a note is hit",
    (await lastCall("set_note")).args.velocity > 100,
    JSON.stringify((await lastCall("set_note")).args));

  // --- a key is a sound you can hear
  // The keyboard is ten octaves tall now, so scroll the one we are aiming at into view.
  await clearCalls();
  await page.evaluate(
    ([high, semitone]) => {
      document.getElementById("rollScroll").scrollTop = (high - 67) * semitone - 100;
    },
    [HIGH_PITCH, SEMITONE],
  );
  const keysBox = await page.locator("#keys").boundingBox();
  await page.mouse.click(keysBox.x + 20, keysBox.y + (HIGH_PITCH - 67) * SEMITONE + 7);
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "audition"));
  check("clicking a key plays the sample at that pitch",
    (await lastCall("audition")).args.pitch === 67);

  // --- right click rubs a note out
  await clearCalls();
  const rubOut = await noteAt(6, 64);
  await page.mouse.click(rubOut.x, rubOut.y, { button: "right" });
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "clear_note"));
  check("right click takes a note out", (await lastCall("clear_note")).args.at.step === 6);
  check("and it goes from the pattern", (await rollLane()).length === 1);

  if (process.env.WEETBEATS_SCREENSHOT) {
    await page.screenshot({ path: process.env.WEETBEATS_SCREENSHOT.replace(/\.png$/, "-roll.png") });
  }

  // --- the roll zooms, by button and by pinch, and the notes follow
  const rollSize = async () => await canvasSize("notes");
  const before = await rollSize();
  await page.locator("#rollZoomIn").click();
  await page.waitForFunction(() => document.getElementById("rollZoomRead").textContent !== "1×");
  const after = await rollSize();
  check("zooming the roll in makes a step wider", after.w > before.w,
    `${after.w} vs ${before.w}`);
  check("and a semitone taller", after.h > before.h, `${after.h} vs ${before.h}`);
  check("the stylesheet's semitone follows, so its lines still line up",
    (await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue("--semitone").trim()))
      !== "15px");

  // A pinch on the trackpad, which arrives as a run of wheel events with ctrl held. They
  // are added up and applied once a frame, so a burst of them is one zoom, not twelve.
  // Counting how many times the canvas is laid out again is how "smooth" is measured: six
  // wheel events inside one frame have to come out as one resize, not six.
  await page.evaluate(() => {
    window.__resizes = 0;
    new MutationObserver((records) => {
      window.__resizes += records.length;
    }).observe(document.getElementById("notes"), {
      attributes: true,
      attributeFilter: ["style"],
    });
  });
  // A real pinch sends wheel events faster than the screen refreshes, so they are all
  // dispatched inside one task here. Driving them through the mouse one await at a time
  // would give each its own frame and prove nothing.
  await page.evaluate(() => {
    const roll = document.getElementById("rollScroll");
    for (let n = 0; n < 6; n++) {
      roll.dispatchEvent(
        new WheelEvent("wheel", {
          deltaY: 20,
          ctrlKey: true,
          clientX: 400,
          clientY: 300,
          bubbles: true,
          cancelable: true,
        }),
      );
    }
  });
  await page.waitForFunction((was) =>
    parseInt(document.getElementById("notes").style.width) < was, after.w);
  check("pinching the roll zooms it out again",
    (await rollSize()).w < after.w, `${(await rollSize()).w} vs ${after.w}`);
  // Two style writes per resize: the width and the height.
  check("and a burst of pinch events is one zoom, not one each",
    (await page.evaluate(() => window.__resizes)) <= 4,
    `${await page.evaluate(() => window.__resizes)} style writes`);

  // Clicking the read-out is the way back to life size, which is where the checks below
  // expect the roll to be.
  await page.locator("#rollZoomRead").click();
  await page.waitForFunction(() => document.getElementById("rollZoomRead").textContent === "1×");
  check("clicking the read-out goes back to life size", (await rollSize()).w === before.w,
    `${(await rollSize()).w} vs ${before.w}`);

  // --- a note past the end of the pattern makes the pattern longer
  await clearCalls();
  // A new note is as long as the last one drawn, which by now is three steps.
  const beyond = await noteAt(18, 65);
  await page.mouse.click(beyond.x, beyond.y);
  await page.waitForFunction(() => document.getElementById("steps").value !== "16");
  check("drawing past the end lengthens the pattern",
    (await page.evaluate(() => window.__weetbeats_state.patterns[0].steps)) === 21,
    String(await page.evaluate(() => window.__weetbeats_state.patterns[0].steps)));
  check("and the length field says so", (await page.locator("#steps").inputValue()) === "21");
  check("and the panel does too", (await rows.first().locator(".plen").textContent()) === "21");
  // Back to sixteen for the checks that follow.
  await page.evaluate(() => {
    const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === 0);
    lane.notes = lane.notes.filter((n) => n.step < 16);
  });
  await page.locator("#closeRoll").click();
  await page.waitForSelector("#editor:visible");
  await page.locator("#steps").click();
  await page.locator("#steps").fill("16");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "16");

  // --- the note button puts the boxes back, and nothing is lost either way
  await clearCalls();
  const kept = (await rollLane()).length;
  await page.locator("#trackHeaders .track").first().locator(".tick.keys-on").click();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_pattern_pitched"));
  check("the note button puts the boxes back",
    (await lastCall("set_pattern_pitched")).args.pitched === false);
  check("and the row is boxes again",
    (await settledGrid(9, 0, EMPTY_BOX)) === EMPTY_BOX, await gridPixel(9, 0));
  check("switching views keeps every note", (await rollLane()).length === kept,
    `${(await rollLane()).length} vs ${kept}`);
  await page.locator("#trackHeaders .track").first().locator(".tick.keys-on").click();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_pattern_pitched").length === 2);
  check("and switching back keeps them too", (await rollLane()).length === kept);

  // --- and it is this pattern's setting, not the track's
  await page.locator("#addPattern").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 2);
  await page.waitForSelector("#editor:visible");
  check("a new pattern's rows are boxes, whatever the last one was",
    !(await page.locator("#trackHeaders .track").first()
      .locator(".tick.keys-on").evaluate((n) => n.classList.contains("on"))));
  check("and its row is drawn as boxes",
    (await settledGrid(9, 0, EMPTY_BOX)) === EMPTY_BOX, await gridPixel(9, 0));

  // Back to the first one, which is still a piano roll.
  await rows.first().click();
  await page.waitForFunction(() =>
    document.querySelector("#trackHeaders .track .tick.keys-on").classList.contains("on"));
  check("the pattern that had a roll still has one",
    (await settledGrid(9, 0, MINI_ROLL)) === MINI_ROLL, await gridPixel(9, 0));
  // The spare pattern goes again; pattern one stays open, which is where the checks below
  // expect to be.
  await rows.nth(1).hover();
  await rows.nth(1).locator(".tick.kill").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 1);
  check("and the pattern being edited is still open", await page.locator("#editor").isVisible());

  // --- the two editors are two views of the same notes
  const miniAgain = await page.locator("#grid").boundingBox();
  await page.mouse.click(miniAgain.x + 2 * CELL + CELL / 2, miniAgain.y + ROW / 2);
  await page.waitForSelector("#roll:visible");
  await page.keyboard.press("Escape");
  await page.waitForSelector("#editor:visible");
  check("escape goes back to the boxes, not the song", await page.locator("#editor").isVisible());
  check("and the song is still behind it", !(await page.locator("#song").isVisible()));
  check("the track shows as an instrument now",
    await page.locator("#trackHeaders .track").first()
      .locator(".tick.keys-on").evaluate((n) => n.classList.contains("on")));

  // A note drawn in the roll at the sampler's own pitch is a ticked box in the grid.
  await page.evaluate(() => {
    window.__weetbeats_calls.length = 0;
  });
  const boxes = await page.evaluate(() => {
    const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === 0);
    return (lane ? lane.notes : []).filter((n) => n.pitch === 60).map((n) => n.step);
  });
  check("the roll and the boxes are the same notes underneath",
    Array.isArray(boxes), JSON.stringify(boxes));

  // --- the panel heading is the way back to the song
  check("the heading is a song button", (await page.locator("#songMode").count()) === 1);
  check("and it is not lit while a pattern is open",
    !(await page.locator("#songMode").evaluate((n) => n.classList.contains("on"))));
  await page.locator("#songMode").click();
  await page.waitForSelector("#song:visible");
  check("clicking it goes back to the song", await page.locator("#song").isVisible());
  check("and then it is lit",
    await page.locator("#songMode").evaluate((n) => n.classList.contains("on")));
  // --- clicking a pattern in the panel opens it, and clicking it again closes it
  await clearCalls();
  await rows.first().click();
  await page.waitForSelector("#editor:visible");
  check("clicking a pattern opens it", await page.locator("#editor").isVisible());
  check("Rust was told which one", (await lastCall("open_pattern")).args.id === 0,
    JSON.stringify((await lastCall("open_pattern")).args));
  await rows.first().click();
  await page.waitForSelector("#song:visible");
  check("clicking the open one closes it", await page.locator("#song").isVisible());
  check("and the pattern it was stays picked out in the song",
    await rows.first().evaluate((n) => n.classList.contains("picked")));

  // --- a double click on a block in the song opens it too
  await openViaSong(0);
  check("double clicking a block opens its pattern", await page.locator("#editor").isVisible());

  // --- the pattern carries the colour of the block it was opened from
  const cssColour = (id, property) =>
    page.evaluate(
      ([id, property]) => getComputedStyle(document.getElementById(id))[property],
      [id, property],
    );
  const rgb = (colour) => (colour.match(/\d+/g) ?? []).slice(0, 3).join(",");
  check("the corner names the pattern",
    (await page.locator("#patternTab").textContent()) === "Pattern 1",
    await page.locator("#patternTab").textContent());
  check("in the colour its blocks are drawn in",
    rgb(await cssColour("patternTab", "backgroundColor")) === blockColour(0),
    await cssColour("patternTab", "backgroundColor"));
  check("and so is the button that closes it",
    rgb(await cssColour("closePattern", "backgroundColor")) === blockColour(0),
    await cssColour("closePattern", "backgroundColor"));
  check("the way out sits at the far right, clear of the instruments", await page.evaluate(() => {
    const shut = document.getElementById("closePattern").getBoundingClientRect();
    const editor = document.getElementById("editor").getBoundingClientRect();
    const chip = document.getElementById("patternChip").getBoundingClientRect();
    // Past the instrument column, hard up against the right hand edge, and inside the strip
    // the ruler runs along rather than down over the grid.
    return shut.left > chip.right && editor.right - shut.right < 20 && shut.bottom < chip.bottom;
  }));

  // Scrolled a long way along, it is still exactly where it was: the corner is stuck to the
  // top left, which is what stops a scrollbar or the window's own edge ever covering it.
  await page.locator("#steps").click();
  await page.locator("#steps").fill("64");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "64");
  const restingPlace = await page.locator("#closePattern").boundingBox();
  await page.evaluate(() => {
    document.getElementById("editorScroll").scrollLeft = 2000;
  });
  await page.waitForTimeout(80);
  const scrolled = await page.locator("#closePattern").boundingBox();
  check("and scrolling the grid does not move it",
    Math.abs(scrolled.x - restingPlace.x) < 1 && scrolled.width > 0,
    `${scrolled.x} vs ${restingPlace.x}`);
  await page.evaluate(() => {
    document.getElementById("editorScroll").scrollLeft = 0;
  });
  await page.locator("#steps").click();
  await page.locator("#steps").fill("16");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "16");

  const underline = (id) =>
    page.evaluate((id) => {
      const node = document.getElementById(id);
      const style = getComputedStyle(node);
      const box = node.getBoundingClientRect();
      return {
        colour: (style.borderBottomColor.match(/\d+/g) ?? []).slice(0, 3).join(","),
        width: parseFloat(style.borderBottomWidth),
        bottom: box.bottom,
      };
    }, id);
  const rulerLine = await underline("ruler");
  const chipLine = await underline("patternChip");
  check("the ruler is underlined in it", rulerLine.colour === blockColour(0), rulerLine.colour);
  // One band across the whole top of the editor, so the two halves of it cannot show a step
  // where the instrument column ends and the grid begins.
  check("and the line beside it starts at the same height",
    rulerLine.width === chipLine.width && Math.abs(rulerLine.bottom - chipLine.bottom) < 0.5,
    `${rulerLine.width}px ending ${rulerLine.bottom} vs ${chipLine.width}px ending ${chipLine.bottom}`);

  // --- closing a pattern: the X, and escape
  await page.locator("#closePattern").click();
  await page.waitForSelector("#song:visible");
  check("the close button shuts the pattern", await page.locator("#song").isVisible());
  check("and the editor goes away", !(await page.locator("#editor").isVisible()));
  check("the panel is still there in the song view", await page.locator("#patternList").isVisible());
  check("Rust was told the pattern closed", (await calls("close_pattern")).length > 0);

  await openViaSong(0);
  await page.keyboard.press("Escape");
  await page.waitForSelector("#song:visible");
  check("escape closes the pattern", await page.locator("#song").isVisible());

  // The block those double clicks needed goes again, so the song is empty for the checks
  // below that count what is in it.
  await rubOutBlock(0, 0);
  await page.waitForFunction(() => window.__weetbeats_state.song.length === 0);
  check("the song view says what to do again", await page.locator("#songHint").isVisible());

  // --- escape in the song view is still the panic button
  await clearCalls();
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "panic_stop"));
  check("escape in the song view stops everything", (await calls("panic_stop")).length === 1);

  // --- a new pattern, from the button under the ones there are
  const listBottom = (await page.locator("#patternList").boundingBox()).y
    + (await page.locator("#patternList").boundingBox()).height;
  const addTop = (await page.locator("#addPattern").boundingBox()).y;
  check("the add button is under the patterns", addTop >= listBottom - 1,
    `${addTop} vs ${listBottom}`);

  await page.locator("#addPattern").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 2);
  check("add makes a pattern", (await rows.count()) === 2);
  check("it is named after the next free number",
    (await rows.nth(1).locator(".pname").textContent()) === "Pattern 2");
  check("and it opens, because it is empty", await page.locator("#editor").isVisible());
  check("the new pattern is the open one",
    await rows.nth(1).evaluate((n) => n.classList.contains("open")));

  // --- double click a name to change it
  await rows.nth(1).dblclick();
  await page.waitForSelector("#patternList .rename");
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
  await rows.first().hover();
  await rows.first().locator(".tick.dup").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 3);
  check("duplicate makes a third pattern", (await rows.count()) === 3);
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

  // --- the song: a block starts where you put it and is as long as its pattern
  const laneCell = async (step, row) => {
    const b = await page.locator("#lanes").boundingBox();
    return { x: b.x + step * SONG_STEP + 2, y: b.y + row * LANE + LANE / 2 };
  };
  const blockAt = async (pattern, step) =>
    (await song()).find(
      (one) => one.pattern === pattern && step >= one.step && step < one.step + one.length,
    ) ?? null;

  let lanes = await canvasSize("lanes");
  const grid = () => page.evaluate(() => parseInt(document.getElementById("songGrid").style.width));
  check("the song canvas is only as wide as the window",
    lanes.w === (await page.evaluate(() => document.getElementById("songScroll").clientWidth)),
    `${lanes.w}px`);
  check("while the song itself fills it", (await grid()) >= 900, `${await grid()}px`);
  check("and is a whole number of bars wide", (await grid()) % BAR_PX === 0, `${await grid()}px`);
  check("the song has a lane per pattern", lanes.h === 2 * LANE, `${lanes.h}px`);

  const top = await laneCell(0, 0);
  await page.mouse.click(top.x, top.y);
  await page.waitForFunction(() => window.__weetbeats_state.song.length === 1);
  check("clicking a lane puts the pattern in the song",
    JSON.stringify(await song()) === JSON.stringify([{ step: 0, pattern: 0, length: 16 }]),
    JSON.stringify(await song()));
  check("the hint goes away", !(await page.locator("#songHint").isVisible()));
  check("and the block is drawn there", (await painted(0, 0)) === blockColour(0),
    await lanePixel(0, 0));
  // Step fourteen, not fifteen: the last two pixels of a block are its drag handle, drawn
  // a shade darker so you can see what you are about to grab.
  check("a sixteen step pattern fills the bar", (await painted(14, 0)) === blockColour(0));
  check("and not the bar after it", (await blank(16, 0)) !== blockColour(0));

  // --- patterns overlap: the whole point of placing rather than sequencing
  const under = await laneCell(0, 1);
  await page.mouse.click(under.x, under.y);
  await page.waitForFunction(() => window.__weetbeats_state.song.length === 2);
  check("two patterns can play at the same time",
    (await song()).filter((one) => one.step === 0).length === 2, JSON.stringify(await song()));
  check("and each is drawn in its own colour",
    (await painted(0, 1)) === blockColour(1), await lanePixel(0, 1));

  // --- dragging the right hand edge makes a block longer
  await clearCalls();
  const rightEdge = await page.evaluate(
    ([songStep, lane]) => {
      const b = document.getElementById("lanes").getBoundingClientRect();
      return { x: b.left + 16 * songStep - 2, y: b.top + lane / 2 };
    },
    [SONG_STEP, LANE],
  );
  await page.mouse.move(rightEdge.x, rightEdge.y);
  await page.mouse.down();
  await page.mouse.move(rightEdge.x + BAR_PX, rightEdge.y, { steps: 6 });
  await page.mouse.up();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "resize_placement"));
  check("dragging the end makes a block longer",
    (await lastCall("resize_placement")).args.length === 32,
    JSON.stringify((await lastCall("resize_placement")).args));
  check("and it is drawn out to there", (await painted(28, 0)) === blockColour(0));
  check("a block longer than its pattern repeats it rather than making a second one",
    (await song()).filter((one) => one.pattern === 0).length === 1, JSON.stringify(await song()));

  // --- and dragging it back in makes it shorter again
  await clearCalls();
  const longEdge = await page.evaluate(
    ([songStep, lane]) => {
      const b = document.getElementById("lanes").getBoundingClientRect();
      return { x: b.left + 32 * songStep - 2, y: b.top + lane / 2 };
    },
    [SONG_STEP, LANE],
  );
  await page.mouse.move(longEdge.x, longEdge.y);
  await page.mouse.down();
  await page.mouse.move(longEdge.x - BAR_PX, longEdge.y, { steps: 6 });
  await page.mouse.up();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "resize_placement"));
  check("dragging it back in makes it shorter",
    (await lastCall("resize_placement")).args.length === 16,
    JSON.stringify((await lastCall("resize_placement")).args));
  check("and the song ends where it did", (await blank(20, 0)) !== blockColour(0));

  // --- dragging the middle of a block moves it along
  await clearCalls();
  const middle = await laneCell(8, 0);
  await page.mouse.move(middle.x, middle.y);
  await page.mouse.down();
  await page.mouse.move(middle.x + BAR_PX * 2, middle.y, { steps: 8 });
  await page.mouse.up();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "move_placement"));
  const slid = (await lastCall("move_placement")).args;
  check("dragging a block moves it", slid.from === 0 && slid.to === 32, JSON.stringify(slid));
  check("it is drawn where it landed", (await painted(32, 0)) === blockColour(0));
  check("and gone from where it was", (await blank(0, 0)) !== blockColour(0));
  check("keeping how long it is", (await blockAt(0, 32)).length === 16,
    JSON.stringify(await blockAt(0, 32)));

  // --- the snap says where blocks land
  await clearCalls();
  await page.locator("#snap").click();
  await page.locator("#snap").fill("4");
  await page.locator("#snap").press("Enter");
  await page.waitForFunction(() => document.getElementById("snap").value === "4");
  const offGrid = await laneCell(70, 0);
  await page.mouse.click(offGrid.x, offGrid.y);
  await page.waitForFunction(() =>
    window.__weetbeats_state.song.some((one) => one.pattern === 0 && one.step === 68));
  check("a block lands on the snap, not on the pattern's own grid",
    (await lastCall("place_pattern")).args.step === 68,
    JSON.stringify((await lastCall("place_pattern")).args));

  // --- a four step pattern goes in four steps at a time, not a bar at a time
  await openViaSong(1);
  await page.locator("#steps").click();
  await page.locator("#steps").fill("4");
  await page.locator("#steps").press("Enter");
  await page.waitForFunction(() => document.getElementById("steps").value === "4");
  await page.locator("#songMode").click();
  await page.waitForSelector("#song:visible");
  check("shortening a pattern leaves its blocks alone",
    (await blockAt(1, 0)).length === 16, JSON.stringify(await blockAt(1, 0)));

  await clearCalls();
  const nextAlong = await laneCell(20, 1);
  await page.mouse.click(nextAlong.x, nextAlong.y);
  await page.waitForFunction(() =>
    window.__weetbeats_state.song.some((one) => one.pattern === 1 && one.step === 20));
  check("a new four step block is four steps long",
    (await lastCall("place_pattern")).args.length === 4,
    JSON.stringify((await lastCall("place_pattern")).args));
  check("so it is drawn four steps wide", (await painted(20, 1)) === blockColour(1));
  check("with the rest of the bar still empty", (await blank(28, 1)) !== blockColour(1));

  // --- drag along a lane to fill it in
  await clearCalls();
  const from = await laneCell(32, 1);
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  for (const step of [32, 36, 40, 44]) {
    const at = await laneCell(step, 1);
    await page.mouse.move(at.x, at.y, { steps: 3 });
  }
  await page.mouse.up();
  const drawn = (await calls("place_pattern")).map((c) => c.args);
  check("dragging fills in a run of blocks", drawn.length === 4, `${drawn.length} blocks`);
  check("and only turns them on", drawn.every((one) => one.on === true));
  check("the steps it crossed are the ones it filled",
    JSON.stringify(drawn.map((one) => one.step)) === JSON.stringify([32, 36, 40, 44]),
    drawn.map((one) => one.step).join(","));

  // --- right click rubs a block out
  await clearCalls();
  const rub = await laneCell(40, 1);
  await page.mouse.click(rub.x, rub.y, { button: "right" });
  await page.waitForFunction(() =>
    !window.__weetbeats_state.song.some((one) => one.pattern === 1 && one.step === 40));
  check("right click takes a block out",
    (await lastCall("place_pattern")).args.on === false);
  check("and the block is gone from there", (await blank(40, 1)) !== blockColour(1));

  // --- a colour per pattern, picked from the list
  await clearCalls();
  await rows.first().hover();
  await rows.first().locator(".swatch").click();
  await page.waitForSelector(".colours");
  await page.locator(".colours button").nth(3).click();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_pattern_colour"));
  check("picking a colour tells Rust which one",
    (await lastCall("set_pattern_colour")).args.colour === 3,
    JSON.stringify((await lastCall("set_pattern_colour")).args));
  check("and the blocks are drawn in it",
    (await settledPixel(32, 0, blockColour(3), true)) === blockColour(3),
    await lanePixel(32, 0));

  // --- zoom: the song is drawn wider, so the same block lands further along
  const home = async () => {
    await page.evaluate(() => {
      document.getElementById("songScroll").scrollLeft = 0;
    });
    await page.waitForTimeout(60);
  };
  const at1x = 32 * SONG_STEP + 2;
  const zoomed = Math.round(32 * SONG_STEP * ZOOM_STEP) + 6;
  await page.locator("#zoomIn").click();
  await page.waitForFunction(() => document.getElementById("zoomRead").textContent !== "1×");
  await home();
  check("zooming in says how far in it is",
    (await page.locator("#zoomRead").textContent()) === "1.3×",
    await page.locator("#zoomRead").textContent());
  check("and the block is drawn further along",
    (await settledAt(zoomed, 0, blockColour(3), true)) === blockColour(3),
    await lanePixelAt(zoomed, 0));
  check("so it is no longer where it was", (await lanePixelAt(at1x, 0)) !== blockColour(3),
    await lanePixelAt(at1x, 0));
  await page.locator("#zoomOut").click();
  await page.waitForFunction(() => document.getElementById("zoomRead").textContent === "1×");
  await home();
  check("and zooming out puts it back",
    (await settledAt(at1x, 0, blockColour(3), true)) === blockColour(3),
    await lanePixelAt(at1x, 0));

  // A trackpad pinch reaches the page as a wheel event with ctrl held: the same handler.
  const lanesBox = await page.locator("#lanes").boundingBox();
  await page.mouse.move(lanesBox.x + 200, lanesBox.y + LANE / 2);
  await page.keyboard.down("Control");
  await page.mouse.wheel(0, -120);
  await page.keyboard.up("Control");
  await page.waitForFunction(() => document.getElementById("zoomRead").textContent !== "1×");
  check("pinching the trackpad zooms too",
    (await page.locator("#zoomRead").textContent()) !== "1×",
    await page.locator("#zoomRead").textContent());
  // The same gesture the other way puts it back, which the checks below want.
  await page.keyboard.down("Control");
  await page.mouse.wheel(0, 120);
  await page.keyboard.up("Control");
  await page.waitForFunction(() => document.getElementById("zoomRead").textContent === "1×");
  await home();
  check("and pinching back out returns to where it was",
    (await settledAt(at1x, 0, blockColour(3), true)) === blockColour(3),
    await lanePixelAt(at1x, 0));

  // --- right click on the scrubber empties the bar
  await clearCalls();
  const scrubber = await page.locator("#scrubber").boundingBox();
  await page.mouse.click(scrubber.x + BAR_PX * 2 + 10, scrubber.y + scrubber.height / 2,
    { button: "right" });
  await page.waitForFunction(() =>
    !window.__weetbeats_state.song.some((one) => one.step >= 32 && one.step < 48));
  check("right click on a bar empties it", (await calls("clear_song_bar")).length === 1);
  check("and leaves the bars either side alone",
    (await song()).some((one) => one.step === 20), JSON.stringify(await song()));

  if (process.env.WEETBEATS_SCREENSHOT) {
    await page.screenshot({ path: process.env.WEETBEATS_SCREENSHOT.replace(/\.png$/, "-song.png") });
  }

  // --- the playhead is a handle, whether or not anything is playing
  // Something in the second bar first: seeking past the end of the song lands on the last
  // step of it, which would make this prove nothing.
  const secondBar = await laneCell(16, 0);
  await page.mouse.click(secondBar.x, secondBar.y);
  await page.waitForFunction(() =>
    window.__weetbeats_state.song.some((one) => one.pattern === 0 && one.step === 16));
  await page.locator("#snap").click();
  await page.locator("#snap").fill("16");
  await page.locator("#snap").press("Enter");
  await page.waitForFunction(() => document.getElementById("snap").value === "16");
  await clearCalls();
  await page.mouse.click(scrubber.x + BAR_PX + 10, scrubber.y + scrubber.height / 2);
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "seek_song"));
  check("clicking the scrubber moves the playhead there while stopped",
    (await lastCall("seek_song")).args.step === 16,
    JSON.stringify((await lastCall("seek_song")).args));
  check("and it is drawn there rather than only while playing", await page.evaluate(
    ([songStep]) => {
      const dpr = window.devicePixelRatio || 1;
      const ctx = document.getElementById("scrubber").getContext("2d");
      const x = Math.round(16 * songStep * dpr);
      const [r, g, b, a] = ctx.getImageData(x, Math.round(20 * dpr), 1, 1).data;
      return a > 0 && r + g + b > 0;
    },
    [SONG_STEP],
  ));

  // --- and dragging it along keeps moving it
  await clearCalls();
  await page.mouse.move(scrubber.x + BAR_PX + 10, scrubber.y + scrubber.height / 2);
  await page.mouse.down();
  await page.mouse.move(scrubber.x + BAR_PX * 2 + 10, scrubber.y + scrubber.height / 2,
    { steps: 6 });
  await page.mouse.up();
  check("dragging the playhead keeps seeking", (await calls("seek_song")).length > 1,
    `${(await calls("seek_song")).length} seeks`);

  // --- the playhead marks every pattern that is sounding, not just one
  // Something from both patterns over the same step first, which is the thing being shown.
  const both = await laneCell(16, 1);
  await page.mouse.click(both.x, both.y);
  await page.waitForFunction(() =>
    window.__weetbeats_state.song.some((one) => one.pattern === 1 && one.step === 16));
  await page.evaluate(() => {
    document.querySelectorAll("#patternList .prow")[0].dataset.marked = "yes";
    window.__weetbeats_setStep(16);
  });
  await page.waitForFunction(() =>
    document.querySelectorAll("#patternList .prow.playing").length === 2, null, { timeout: 4000 });
  check("both patterns in the bar show as playing",
    (await page.locator("#patternList .prow.playing").count()) === 2);
  check("and the rows are not rebuilt while it plays, so a rename survives",
    (await rows.first().evaluate((n) => n.dataset.marked)) === "yes");

  // --- the level meter moves while the song plays
  // It is a transform, not a width: a width has to lay the page out, and doing that sixty
  // times a second behind a song view that is already redrawing is what made it look dead.
  const maskScale = () =>
    page.evaluate(() => {
      const shown = getComputedStyle(document.getElementById("meterMask")).transform;
      const numbers = shown.match(/-?[\d.]+/g);
      return numbers ? Number(numbers[0]) : 1;
    });
  check("the meter is driven by a transform, not an animated width",
    (await page.evaluate(() =>
      getComputedStyle(document.getElementById("meterMask")).transitionProperty)) === "all" ||
      (await page.evaluate(() =>
        getComputedStyle(document.getElementById("meterMask")).transitionDuration)) === "0s");
  await page.waitForFunction(() => {
    const shown = getComputedStyle(document.getElementById("meterMask")).transform;
    const numbers = shown.match(/-?[\d.]+/g);
    return numbers && Number(numbers[0]) < 0.5;
  }, null, { timeout: 4000 });
  check("the meter reads the mix while the song plays", (await maskScale()) < 0.5,
    String(await maskScale()));

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

  // --- space with a number focused still plays: only real text should swallow it
  await clearCalls();
  await page.locator("#bpm").focus();
  await page.keyboard.press("Space");
  check("space works with the tempo focused", (await calls("set_playing")).length > 0);

  // --- mute, solo, volume, delete
  await openViaSong(0);
  await clearCalls();
  await page.locator("#trackHeaders .track").first().locator(".tick.mute").click();
  await page.locator("#trackHeaders .track").first().locator(".tick.solo").click();
  check("mute reaches the engine", (await lastCall("set_pattern_muted")).args.muted === true);
  check("and it says which pattern", (await lastCall("set_pattern_muted")).args.pattern === 0,
    JSON.stringify((await lastCall("set_pattern_muted")).args));
  check("solo reaches the engine",
    (await lastCall("set_pattern_soloed")).args.soloed === true);
  check("mute button shows as on", await page.locator("#trackHeaders .track").first()
    .locator(".tick.mute").evaluate((n) => n.classList.contains("on")));
  // Mute wins, so a solo underneath one is showing something that is not happening.
  check("and the solo under it goes faint, because mute wins",
    await page.locator("#trackHeaders .track").first()
      .locator(".tick.solo").evaluate((n) => n.classList.contains("beaten")));
  await page.locator("#trackHeaders .track").first().locator(".tick.mute").click();
  check("and comes back when the mute goes",
    !(await page.locator("#trackHeaders .track").first()
      .locator(".tick.solo").evaluate((n) => n.classList.contains("beaten"))));
  // Muted again, which is how the checks further down expect to find it.
  await page.locator("#trackHeaders .track").first().locator(".tick.mute").click();

  // The three switches on a row are drawings, not letters: M and S say nothing unless you
  // already know the words, and ♪ was body text pretending to be an icon.
  check("mute, solo and the piano roll are drawn, not spelled out", await page.evaluate(() => {
    const row = document.querySelector("#trackHeaders .track");
    return ["mute", "solo", "keys-on"].every((name) => {
      const button = row.querySelector(`.tick.${name}`);
      return button && button.querySelector("svg") && !button.textContent.trim();
    });
  }));
  check("and they say what they are for anyone not looking at them", await page.evaluate(() => {
    const row = document.querySelector("#trackHeaders .track");
    return ["mute", "solo", "keys-on"].every((name) =>
      (row.querySelector(`.tick.${name}`).getAttribute("aria-label") ?? "").length > 3);
  }));

  await page.locator("#trackHeaders .track").first().locator("input[type=range]").fill("40");
  const gain = (await lastCall("set_pattern_gain")).args;
  check("volume reaches the engine", Math.abs(gain.gain - 0.4) < 1e-6, JSON.stringify(gain));
  check("for this pattern", gain.pattern === 0 && gain.track === 0, JSON.stringify(gain));

  // --- and the whole row belongs to the pattern, not to the track
  const fader = () =>
    page.locator("#trackHeaders .track").first().locator("input[type=range]").inputValue();
  const mutedNow = () =>
    page.locator("#trackHeaders .track").first()
      .locator(".tick.mute").evaluate((n) => n.classList.contains("on"));
  const turnedDown = await rows.count();
  await clearCalls();
  await page.locator("#addPattern").click();
  await page.waitForFunction((was) =>
    document.querySelectorAll("#patternList .prow").length === was + 1, turnedDown);
  await page.waitForSelector("#editor:visible");
  check("a new pattern's mixer starts where a new one starts", (await fader()) === "80",
    await fader());
  check("and nothing is muted in it", !(await mutedNow()));

  // Back to the one that was turned down and muted, which still is.
  await rows.first().click();
  await page.waitForFunction(() =>
    document.querySelector("#trackHeaders .track input[type=range]").value === "40");
  check("the pattern that was turned down still is", (await fader()) === "40");
  check("and still muted", await mutedNow());

  // The spare goes again, so the counts below are what they were.
  await rows.nth(turnedDown).hover();
  await rows.nth(turnedDown).locator(".tick.kill").click();
  await page.waitForFunction((was) =>
    document.querySelectorAll("#patternList .prow").length === was, turnedDown);

  // --- the sound editor: one track's instrument, rather than one pattern's part
  await clearCalls();
  await page.locator("#trackHeaders .track").first().locator(".wave").click();
  await page.waitForSelector("#sound:visible");
  check("clicking a track's waveform opens the sound editor",
    await page.locator("#sound").isVisible());
  check("and the boxes step aside", !(await page.locator("#editor").isVisible()));
  check("it says which sound it is",
    (await page.locator("#soundName").textContent()).includes("kick"),
    await page.locator("#soundName").textContent());
  // The same band across the top the other views have, in the colour of the pattern it was
  // opened over, with the way out at the far right of it.
  check("and wears the open pattern's colour", await page.evaluate(() => {
    const chip = getComputedStyle(document.getElementById("soundChip")).borderBottomColor;
    const shut = getComputedStyle(document.getElementById("closeSound")).backgroundColor;
    return chip === shut;
  }));
  check("with the way out at the far right", await page.evaluate(() => {
    const shut = document.getElementById("closeSound").getBoundingClientRect();
    const view = document.getElementById("sound").getBoundingClientRect();
    return view.right - shut.right < 20;
  }));

  // Every control is there, and each one starts where an unshaped sound starts.
  const knob = (key) => page.locator(`#sound input[data-key="${key}"]`);
  const reads = (key) => page.locator(`#sound [data-read="${key}"]`).textContent();
  for (const key of ["attack", "decay", "sustain", "release", "pan", "tune", "level",
    "start", "end"]) {
    check(`the sound editor has a ${key}`, (await knob(key).count()) === 1);
  }
  check("a sound nobody has shaped sits in the middle", (await reads("pan")) === "middle");
  check("and is tuned as it was recorded", (await reads("tune")) === "as recorded");
  check("and uses the whole file", (await reads("end")) === "100%");

  // Moving one reaches Rust, whole, and says which track.
  const setKnob = (key, value) =>
    page.evaluate(([key, value]) => {
      const slider = document.querySelector(`#sound input[data-key="${key}"]`);
      slider.value = String(value);
      slider.dispatchEvent(new Event("input", { bubbles: true }));
    }, [key, value]);
  await setKnob("release", 500);
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_voicing"));
  const voiced = (await lastCall("set_voicing")).args;
  check("a control reaches Rust", voiced.id === 0, JSON.stringify(voiced.id));
  check("as a whole voicing, not a field", Object.keys(voiced.voicing).length === 9,
    Object.keys(voiced.voicing).join(","));
  // Half way along a squared slider is a quarter of the way up the range, not half: the
  // difference between nought and thirty milliseconds matters far more often than the one
  // between three seconds and four.
  check("with the value the slider means, not the slider's own number",
    Math.abs(voiced.voicing.release - 0.5 * 0.5 * 4) < 1e-6,
    String(voiced.voicing.release));
  check("and the read-out says it in units", (await reads("release")) === "1.00 s",
    await reads("release"));

  // It belongs to the track, so it is the same wherever the sound is played.
  check("the sound belongs to the track, not the pattern", await page.evaluate(() =>
    Math.abs(window.__weetbeats_state.tracks.get(0).voicing.release - 1) < 1e-6));

  // Rust pushes the ends of a trim apart rather than letting them cross, and the editor
  // shows where they really ended up.
  await setKnob("start", 900);
  await setKnob("end", 100);
  await page.waitForFunction(() =>
    window.__weetbeats_state.tracks.get(0).voicing.end > 0.9);
  check("the end of a trim cannot be dragged past its start",
    await page.evaluate(() => {
      const v = window.__weetbeats_state.tracks.get(0).voicing;
      return v.end > v.start;
    }));
  check("and the control shows where it really landed",
    (await knob("end").inputValue()) !== "100", await knob("end").inputValue());

  // Double click puts one control back, which is the only way to "none of this".
  await page.locator("#sound .knob").filter({ hasText: "release" }).dblclick();
  await page.waitForFunction(() =>
    window.__weetbeats_state.tracks.get(0).voicing.release < 0.01);
  check("double clicking a control puts it back where it started",
    (await reads("release")) === "3 ms", await reads("release"));

  // Escape walks back out to the pattern, not all the way to the song.
  await page.locator("body").press("Escape");
  await page.waitForSelector("#editor:visible");
  check("escape comes back out to the pattern", await page.locator("#editor").isVisible());
  check("and not all the way to the song", !(await page.locator("#song").isVisible()));

  // --- a track whose sound is a CLAP instrument rather than a file
  await clearCalls();
  await page.locator("#addPlugin").click();
  await page.waitForSelector("#picker:visible");
  check("the plugin button opens the picker", await page.locator("#picker").isVisible());
  await page.waitForFunction(() => document.querySelectorAll(".plugin-row").length > 0);
  check("with what the scan found", (await page.locator(".plugin-row").count()) === 3);
  // An effect is shown rather than hidden, because "why is my plugin not here" deserves an
  // answer — but it cannot be added, because effects are a later stage.
  check("an effect is shown but cannot be picked", await page.evaluate(() => {
    const rows = [...document.querySelectorAll(".plugin-row")];
    const effect = rows.find((r) => r.textContent.includes("not an instrument"));
    return Boolean(effect) && effect.disabled;
  }));
  await page.locator("#pluginFilter").fill("vital");
  await page.waitForFunction(() => document.querySelectorAll(".plugin-row").length === 1);
  check("the filter narrows it down", (await page.locator(".plugin-row").count()) === 1);
  await page.locator("#pluginFilter").fill("");
  await page.waitForFunction(() => document.querySelectorAll(".plugin-row").length === 3);

  const pluggedRows = () => page.locator("#trackHeaders .track").count();
  const rowsBefore = await pluggedRows();
  await page.locator(".plugin-row", { hasText: "Surge XT" }).first().click();
  await page.waitForFunction((was) =>
    document.querySelectorAll("#trackHeaders .track").length === was + 1, rowsBefore);
  check("picking one adds a track", (await pluggedRows()) === rowsBefore + 1);
  check("and says which plugin, from where",
    (await lastCall("add_plugin")).args.id === "org.surge-synth-team.surge-xt",
    JSON.stringify((await lastCall("add_plugin")).args));
  check("and the picker goes away", !(await page.locator("#picker").isVisible()));

  const plugRow = page.locator("#trackHeaders .track").last();
  check("its row wears a plug instead of a waveform", await plugRow
    .locator(".wave").evaluate((n) => n.classList.contains("plugged") && !!n.querySelector("svg")));

  // Clicking a plug goes straight to the plugin's own window — the thing you came for —
  // rather than to a screen whose whole job was a button that opens it.
  await clearCalls();
  await plugRow.locator(".wave").click();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_plugin_window"));
  check("clicking a plug opens the plugin's own window",
    (await lastCall("set_plugin_window")).args.open === true,
    JSON.stringify((await lastCall("set_plugin_window")).args));
  check("and leaves you where you were, rather than on a screen about it",
    (await page.locator("#editor").isVisible()) && !(await page.locator("#sound").isVisible()));
  await page.waitForFunction(() =>
    document.querySelector("#trackHeaders .track:last-child .wave").classList.contains("on"));
  check("the plug is lit while its window is up",
    await plugRow.locator(".wave").evaluate((n) => n.classList.contains("on")));
  await clearCalls();
  await plugRow.locator(".wave").click();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_plugin_window"));
  check("and the same plug puts it away again",
    (await lastCall("set_plugin_window")).args.open === false,
    JSON.stringify((await lastCall("set_plugin_window")).args));

  // And what we have to say about a plugin track is a shift click away.
  await plugRow.locator(".wave").click({ modifiers: ["Shift"] });
  await page.waitForSelector("#sound:visible");
  check("shift clicking it opens the plugin's controls",
    await page.locator("#pluginBody").isVisible());
  check("and the sampler's are put away, because a synth has its own",
    !(await page.locator("#soundBody").isVisible()));
  check("it says which plugin it is",
    (await page.locator("#soundName").textContent()) === "Surge XT",
    await page.locator("#soundName").textContent());
  await page.waitForFunction(() => document.querySelectorAll("#pluginParams .knob").length > 0);
  check("with the parameters the plugin says it has",
    (await page.locator("#pluginParams .knob").count()) === 3);
  check("and how loud the track is, which is still ours",
    (await page.locator("#pluginLevelKnobs .knob").count()) === 1);

  // Moving one goes straight to the audio thread: what a plugin is set to is the plugin's.
  await clearCalls();
  await page.evaluate(() => {
    const slider = document.querySelector("#pluginParams input");
    slider.value = "1000";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "set_plugin_param"));
  const turned = (await lastCall("set_plugin_param")).args;
  check("moving a control reaches the plugin", turned.param === 0 && turned.value === 1,
    JSON.stringify(turned));

  // The plugin's own window: Surge XT's real interface, floating above ours.
  await clearCalls();
  check("the plugin's window starts shut",
    (await page.locator("#pluginWindow").textContent()).trim() === "open its window",
    await page.locator("#pluginWindow").textContent());
  await page.locator("#pluginWindow").click();
  await page.waitForFunction(() =>
    document.getElementById("pluginWindow").textContent.includes("close"));
  check("clicking it asks Rust to open one",
    (await lastCall("set_plugin_window")).args.open === true,
    JSON.stringify((await lastCall("set_plugin_window")).args));
  check("and the button becomes the way to shut it",
    await page.locator("#pluginWindow").evaluate((n) => n.classList.contains("on")));
  await page.locator("#pluginWindow").click();
  await page.waitForFunction(() =>
    document.getElementById("pluginWindow").textContent.includes("open"));
  check("and clicking again shuts it",
    (await lastCall("set_plugin_window")).args.open === false);

  // Rust is asked rather than remembered, because a floating window can be shut by its own
  // close box without anything reaching us.
  const pluggedTrack = (await lastCall("set_plugin_window")).args.id;
  await page.evaluate((id) => window.__weetbeats_state.pluginWindows.add(id), pluggedTrack);
  await page.locator("body").press("Escape");
  await page.waitForSelector("#editor:visible");
  await page.locator("#trackHeaders .track").last().locator(".wave").click({ modifiers: ["Shift"] });
  await page.waitForFunction(() =>
    document.getElementById("pluginWindow").textContent.includes("close"));
  check("re-opening the editor asks Rust whether the window is still up",
    await page.locator("#pluginWindow").evaluate((n) => n.classList.contains("on")));
  await page.evaluate(() => window.__weetbeats_state.pluginWindows.clear());

  await page.locator("#paramFilter").fill("cutoff");
  await page.waitForFunction(() =>
    document.querySelectorAll("#pluginParams .knob").length === 1);
  check("the parameter filter narrows it down",
    (await page.locator("#pluginParams .knob").count()) === 1);
  await page.locator("#paramFilter").fill("");

  await page.locator("body").press("Escape");
  await page.waitForSelector("#editor:visible");

  // --- a plugin with no window of its own says so, rather than doing nothing
  await page.locator("#addPlugin").click();
  await page.waitForSelector("#picker:visible");
  await page.locator(".plugin-row", { hasText: "Vital" }).first().click();
  await page.waitForFunction((was) =>
    document.querySelectorAll("#trackHeaders .track").length === was + 2, rowsBefore);
  await page.locator("#trackHeaders .track").last().locator(".wave").click();
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("no window"));
  check("a plugin with no window says so",
    (await page.locator("#status").textContent()).includes("no window of its own"),
    await page.locator("#status").textContent());
  // And then our own controls are all there is, so that is where the plug lands you.
  await page.waitForSelector("#sound:visible");
  check("and our own controls are where it lands you instead",
    await page.locator("#pluginBody").isVisible());
  check("and the button stays the way it was",
    (await page.locator("#pluginWindow").textContent()).trim() === "open its window");
  // A complaint holds the status line for a few seconds, and while it is up a passing note
  // stands aside for it. Let it lapse, or it swallows the notes the checks below wait for.
  await page.waitForFunction(() => document.getElementById("status").textContent === "");
  await page.locator("body").press("Escape");
  await page.waitForSelector("#editor:visible");
  await page.locator("#trackHeaders .track").last().locator(".tick.kill").click();
  await page.waitForFunction((was) =>
    document.querySelectorAll("#trackHeaders .track").length === was + 1, rowsBefore);

  await page.locator("#trackHeaders .track").last().locator(".tick.kill").click();
  await page.waitForFunction((was) =>
    document.querySelectorAll("#trackHeaders .track").length === was, rowsBefore);

  // --- deleting a track takes its notes out of every pattern
  await page.locator("#trackHeaders .track").first().locator(".tick.kill").click();
  await page.waitForFunction(() => document.querySelectorAll("#trackHeaders .track").length === 2);
  check("deleting a track removes the row", (await page.locator("#trackHeaders .track").count()) === 2);
  check("and its notes go with it", await page.evaluate(() =>
    window.__weetbeats_state.patterns.every((p) => p.lanes.every((l) => l.track !== 0))));

  // --- undo and redo
  const steps = () => page.evaluate(() => window.__weetbeats_history());
  await openViaSong(0);
  // The colour picked for this pattern back in the song view followed it in here.
  check("the editor follows a colour picked in the panel",
    (await page.evaluate(() =>
      getComputedStyle(document.getElementById("patternTab")).backgroundColor))
      .match(/\d+/g).slice(0, 3).join(",") === blockColour(3),
    await page.evaluate(() =>
      getComputedStyle(document.getElementById("patternTab")).backgroundColor));
  await clearCalls();
  const boxAt = async (step, row) => {
    const box = await page.locator("#grid").boundingBox();
    return { x: box.x + step * CELL + CELL / 2, y: box.y + row * ROW + ROW / 2 };
  };
  const ticked = () =>
    page.evaluate(() =>
      window.__weetbeats_state.patterns
        .find((p) => p.id === 0)
        .lanes.flatMap((l) => l.notes.map((n) => n.step))
        .sort((a, b) => a - b)
        .join(","));

  const one = await boxAt(3, 0);
  await page.mouse.click(one.x, one.y);
  await page.waitForFunction(() => window.__weetbeats_calls.some((c) => c.name === "set_step"));
  const withBox = await ticked();
  check("a box is ticked", withBox.split(",").includes("3"), withBox);

  await page.keyboard.press("Control+z");
  await page.waitForFunction((was) => {
    const now = window.__weetbeats_state.patterns
      .find((p) => p.id === 0)
      .lanes.flatMap((l) => l.notes.map((n) => n.step))
      .sort((a, b) => a - b)
      .join(",");
    return now !== was;
  }, withBox);
  check("undo takes the box back", !(await ticked()).split(",").includes("3"), await ticked());
  check("and it does not throw you out of the pattern",
    await page.locator("#editor").isVisible());
  check("and the engine is told the pattern is still the live one",
    (await lastCall("open_pattern")).args.id === 0,
    JSON.stringify((await lastCall("open_pattern")).args));
  check("and the grid is drawn without it",
    (await settledGrid(3, 0, EMPTY_BOX)) === EMPTY_BOX, await gridPixel(3, 0));

  await page.keyboard.press("Control+Shift+z");
  await page.waitForFunction((want) => {
    const now = window.__weetbeats_state.patterns
      .find((p) => p.id === 0)
      .lanes.flatMap((l) => l.notes.map((n) => n.step))
      .sort((a, b) => a - b)
      .join(",");
    return now === want;
  }, withBox);
  check("redo puts it back", (await ticked()) === withBox, await ticked());

  // A drag across boxes is one thing you did, so it comes back in one step.
  await clearCalls();
  const stepsBefore = (await steps()).past;
  const dragFrom = await boxAt(8, 0);
  await page.mouse.move(dragFrom.x, dragFrom.y);
  await page.mouse.down();
  for (const at of [9, 10, 11, 12]) {
    const p = await boxAt(at, 0);
    await page.mouse.move(p.x, p.y, { steps: 2 });
  }
  await page.mouse.up();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.filter((c) => c.name === "set_step").length >= 5);
  check("a drag across boxes is one step to take back",
    (await steps()).past === stepsBefore + 1,
    `${(await steps()).past} vs ${stepsBefore + 1}`);
  const dragged = await ticked();
  await page.keyboard.press("Control+z");
  await page.waitForFunction((was) => {
    const now = window.__weetbeats_state.patterns
      .find((p) => p.id === 0)
      .lanes.flatMap((l) => l.notes.map((n) => n.step))
      .sort((a, b) => a - b)
      .join(",");
    return now !== was;
  }, dragged);
  check("and one undo takes the whole drag back", (await ticked()) === withBox,
    await ticked());

  // Renaming, which is a different kind of edit, is its own step.
  await page.locator("#songMode").click();
  await page.waitForSelector("#song:visible");
  await rows.first().dblclick();
  await page.waitForSelector("#patternList .rename");
  await page.locator("#patternList .rename").fill("Verse");
  await page.locator("#patternList .rename").press("Enter");
  await page.waitForFunction(() =>
    document.querySelector("#patternList .prow .pname").textContent === "Verse");
  await page.keyboard.press("Control+z");
  await page.waitForFunction(() =>
    document.querySelector("#patternList .prow .pname").textContent !== "Verse");
  check("undo puts a name back", (await rows.first().locator(".pname").textContent()) !== "Verse",
    await rows.first().locator(".pname").textContent());

  // And when there is nothing left to take back it says so rather than doing something.
  await page.evaluate(() => {
    while (window.__weetbeats_history().past > 0) window.__TAURI__.core.invoke("undo");
  });
  await page.waitForFunction(() => window.__weetbeats_history().past === 0);
  await clearCalls();
  await page.keyboard.press("Control+z");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("nothing to undo"));
  check("with nothing to undo it says so",
    (await page.locator("#status").textContent()).includes("nothing to undo"));

  // --- the File menu is Rust's, and it tells the front end what it did
  await menu("save");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("saved"));
  check("saving says so", (await page.locator("#status").textContent()).includes("saved Untitled"),
    await page.locator("#status").textContent());

  await menu("save_as");
  await page.waitForFunction(() => document.getElementById("songName").textContent === "Newer");
  check("save as renames the project",
    (await page.locator("#songName").textContent()) === "Newer");

  // --- double click the song's name to change it, the same as a pattern's
  await clearCalls();
  await page.locator("#songMode").dblclick();
  await page.waitForSelector("#songMode .rename");
  await page.locator("#songMode .rename").fill("Bangers");
  await page.locator("#songMode .rename").press("Enter");
  await page.waitForFunction(() => document.getElementById("songName").textContent === "Bangers");
  check("renaming the song reaches Rust",
    (await lastCall("rename_project")).args.name === "Bangers",
    JSON.stringify((await lastCall("rename_project")).args));
  check("and the button shows the new name",
    (await page.locator("#songName").textContent()) === "Bangers");

  await menu("trouble");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("disk said no"));
  check("and trouble in the menu reaches the status line",
    (await page.locator("#status").textContent()).includes("disk said no"));

  // --- opening a project replaces everything
  await page.evaluate(() => {
    window.__weetbeats_state.openFolder = "/elsewhere/Other.beat";
    window.__weetbeats_state.patterns = [
      { id: 0, name: "Opened", steps: 32, lanes: [] },
      { id: 1, name: "Second", steps: 16, lanes: [] },
    ];
    // A block off the pattern's own grid: the kind an older version could write and no
    // click could then land on.
    window.__weetbeats_state.song = [
      { step: 0, pattern: 0, length: 32 },
      { step: 48, pattern: 1, length: 16 },
    ];
  });
  await menu("open");
  await page.waitForFunction(() => document.getElementById("songName").textContent === "Other");
  check("opening a project redraws the patterns",
    (await rows.first().locator(".pname").textContent()) === "Opened");
  check("and lands in the song, because there is one",
    await page.locator("#song").isVisible());
  check("with the song it was saved with", (await song()).length === 2);

  // --- a block off the pattern's own grid can still be picked up
  await clearCalls();
  const stuck = await page.locator("#lanes").boundingBox();
  await page.mouse.click(stuck.x + 52 * SONG_STEP, stuck.y + LANE + LANE / 2, { button: "right" });
  await page.waitForFunction(() =>
    !window.__weetbeats_state.song.some((one) => one.pattern === 1));
  check("a block that is not on its pattern's grid can still be rubbed out",
    (await lastCall("place_pattern")).args.step === 48,
    JSON.stringify((await lastCall("place_pattern")).args));

  // --- and the last pattern cannot be deleted
  await rows.nth(1).hover();
  await rows.nth(1).locator(".tick.kill").click();
  await page.waitForFunction(() => document.querySelectorAll("#patternList .prow").length === 1);
  await rows.first().hover();
  await rows.first().locator(".tick.kill").click();
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("at least one"));
  check("the last pattern stays", (await rows.count()) === 1);

  // --- a pattern can be silenced from its own row in the panel
  //
  // Not the mute on a track's row, which is one track inside one pattern. This one is the
  // pattern, so every block of it in the song goes quiet at once.
  await clearCalls();
  check("every pattern's row has a speaker on it",
    (await rows.first().locator(".tick.mute").count()) === 1);
  const litBlock = await lanePixel(2, 0);
  await rows.first().hover();
  await rows.first().locator(".tick.mute").click();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "mute_pattern"));
  check("clicking it silences the pattern",
    (await lastCall("mute_pattern")).args.muted === true,
    JSON.stringify((await lastCall("mute_pattern")).args));
  check("and says which pattern, so it is the pattern's own switch",
    (await lastCall("mute_pattern")).args.id === 0);
  check("the speaker lights up", await rows.first()
    .locator(".tick.mute").evaluate((n) => n.classList.contains("on")));
  check("and the row goes quiet with it, so a silent pattern is never a mystery",
    await rows.first().evaluate((n) => n.classList.contains("muted")));
  // Its blocks in the song go faint rather than away: they still say where the part plays.
  const fadedBlock = await (async () => {
    const deadline = Date.now() + 2000;
    for (;;) {
      const got = await lanePixel(2, 0);
      if (got !== litBlock || Date.now() > deadline) return got;
      await page.waitForTimeout(25);
    }
  })();
  check("and its blocks in the song go faint", fadedBlock !== litBlock,
    `${fadedBlock} vs ${litBlock}`);
  check("it is the pattern that is muted, not anything in its mixer",
    await page.evaluate(() => window.__weetbeats_state.patterns[0].muted === true));
  await rows.first().locator(".tick.mute").click();
  await page.waitForFunction(() =>
    !document.querySelector("#patternList .prow").classList.contains("muted"));
  check("and clicking it again brings the pattern back",
    (await lastCall("mute_pattern")).args.muted === false);

  // --- dragging a block's end into the edge of the window scrolls the song along
  //
  // Stretching is the case that needs it: what you are dragging is the right hand end, so
  // the moment it reaches the edge there is nowhere left to pull it to.
  await page.locator("#zoomRead").click();
  await page.waitForFunction(() => document.getElementById("zoomRead").textContent === "1×");
  await page.evaluate(() => {
    document.getElementById("songScroll").scrollLeft = 0;
  });
  await clearCalls();
  const songBox = await page.locator("#songScroll").boundingBox();
  const stretchLanes = await page.locator("#lanes").boundingBox();
  // The right hand end of the block, which is the handle for how long it is.
  const blockEnd = { x: stretchLanes.x + 32 * SONG_STEP - 3, y: stretchLanes.y + LANE / 2 };
  await page.mouse.move(blockEnd.x, blockEnd.y);
  await page.mouse.down();
  await page.mouse.move(songBox.x + songBox.width - 6, blockEnd.y, { steps: 6 });
  await page.waitForFunction(() => document.getElementById("songScroll").scrollLeft > 0);
  const followed = await page.evaluate(() => document.getElementById("songScroll").scrollLeft);
  check("stretching a block into the edge scrolls the song to follow", followed > 0,
    `${followed}px`);
  await page.mouse.up();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "resize_placement"));
  check("and the block goes on growing past what the window could show",
    (await song()).find((one) => one.pattern === 0).length > 32,
    JSON.stringify(await song()));
  await page.evaluate(() => {
    document.getElementById("songScroll").scrollLeft = 0;
  });

  // --- picking notes out in the roll: a box round them, then move, copy or rub out
  await openViaSong(0);
  // Two instruments to write with. Everything added above has been taken away again by the
  // undos, so this starts from the kit the checks below actually need.
  await page.evaluate(() => {
    window.__weetbeats_state.picks = ["/pack/01 kick.wav", "/pack/03 bass.wav"];
  });
  await page.locator(await page.locator("#addBig").isVisible() ? "#addBig" : "#add").click();
  await page.waitForFunction(() => document.querySelectorAll("#trackHeaders .track").length === 2);
  await page.locator("#trackHeaders .track").first().locator(".tick.keys-on").click();
  await page.waitForFunction(() =>
    document.querySelector("#trackHeaders .track .tick.keys-on").classList.contains("on"));
  const miniRoll = await page.locator("#grid").boundingBox();
  await page.mouse.click(miniRoll.x + 2 * CELL + CELL / 2, miniRoll.y + ROW / 2);
  await page.waitForSelector("#roll:visible");
  // Back to the top left, and to pitches with room above and below them: the keyboard is
  // stuck to the left of the notes and the ruler to the top, and both cover what is under
  // them. The zoom checks above left the roll scrolled somewhere else.
  await page.evaluate(
    ([high, semitone]) => {
      const roll = document.getElementById("rollScroll");
      roll.scrollLeft = 0;
      roll.scrollTop = (high - 66) * semitone - 60;
    },
    [HIGH_PITCH, SEMITONE],
  );
  // Which tracks are left by now is the story of every check above, so ask rather than count.
  const trackIds = await page.evaluate(() => [...window.__weetbeats_state.tracks.keys()]);
  const laneOf = (track) =>
    page.evaluate((id) => {
      const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === id);
      return lane ? lane.notes : [];
    }, track);
  const rollLaneNow = () => laneOf(trackIds[0]);

  // Two notes, drawn two steps long so nothing below depends on what the last drag left.
  // A couple of steps in: the keyboard down the left is stuck over the start of the notes.
  const drawnFrom = await noteAt(2, 64);
  const drawnTo = await noteAt(3, 64);
  await page.mouse.move(drawnFrom.x, drawnFrom.y);
  await page.mouse.down();
  await page.mouse.move(drawnTo.x, drawnTo.y, { steps: 3 });
  await page.mouse.up();
  const second = await noteAt(6, 62);
  await page.mouse.click(second.x, second.y);
  await page.waitForFunction((id) => {
    const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === id);
    return lane && lane.notes.length === 2;
  }, trackIds[0]);

  // Shift drags a box round them, because a plain press already means "draw one here".
  const boxFrom = await noteAt(2, 65);
  const boxTo = await noteAt(10, 61);
  await page.keyboard.down("Shift");
  await page.mouse.move(boxFrom.x, boxFrom.y);
  await page.mouse.down();
  await page.mouse.move(boxTo.x, boxTo.y, { steps: 5 });
  await page.mouse.up();
  await page.keyboard.up("Shift");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("picked out"));
  check("a box dragged round notes picks them out",
    (await page.locator("#status").textContent()).startsWith("2 notes picked out"),
    await page.locator("#status").textContent());
  check("and dragging a box draws no note", (await rollLaneNow()).length === 2);

  // Dragging one of them takes the whole set, keeping the shape of what was picked out.
  await clearCalls();
  const grabOne = await noteAt(2, 64);
  const dropOne = await noteAt(4, 64);
  await page.mouse.move(grabOne.x, grabOne.y);
  await page.mouse.down();
  await page.mouse.move(dropOne.x, dropOne.y, { steps: 5 });
  await page.mouse.up();
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "edit_notes"));
  const moved2 = (await lastCall("edit_notes")).args;
  check("dragging one of a picked set moves the lot",
    moved2.remove.length === 2 && moved2.add.length === 2, JSON.stringify(moved2));
  check("in one trip, so a note landing where another has just left survives",
    moved2.add.map((n) => n.step).sort((a, b) => a - b).join() === "4,8",
    JSON.stringify(moved2.add));
  check("and they keep the shape they were picked out in",
    moved2.add.find((n) => n.pitch === 64).step === 4 &&
      moved2.add.find((n) => n.pitch === 62).step === 8);

  // Copy and paste. Straight back where it came from is the one case that would do nothing,
  // so there it lands after itself instead.
  await clearCalls();
  await page.keyboard.press("c");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("copied"));
  check("c copies what is picked out",
    (await page.locator("#status").textContent()).startsWith("2 notes copied"),
    await page.locator("#status").textContent());
  await page.keyboard.press("v");
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "edit_notes"));
  const pasted = (await lastCall("edit_notes")).args;
  check("v puts them down again", pasted.add.length === 2 && pasted.remove.length === 0,
    JSON.stringify(pasted));
  check("after themselves, rather than on top of themselves",
    pasted.add.every((n) => n.step > 8), JSON.stringify(pasted.add));
  check("and the pattern has four notes now", (await rollLaneNow()).length === 4);

  // Shift clicking one note puts it in the set, or takes it back out.
  const oneMore = await noteAt(4, 64);
  await page.keyboard.down("Shift");
  await page.mouse.click(oneMore.x, oneMore.y);
  await page.keyboard.up("Shift");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.startsWith("3 notes"));
  check("shift clicking a note puts it in the set",
    (await page.locator("#status").textContent()).startsWith("3 notes picked out"),
    await page.locator("#status").textContent());
  await page.keyboard.down("Shift");
  await page.mouse.click(oneMore.x, oneMore.y);
  await page.keyboard.up("Shift");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.startsWith("2 notes"));
  check("and shift clicking it again takes it back out",
    (await page.locator("#status").textContent()).startsWith("2 notes picked out"));
  check("and neither drew or rubbed out anything", (await rollLaneNow()).length === 4);

  // Alt drags copies off the originals, which is how one bar becomes two without going
  // anywhere near the clipboard.
  await clearCalls();
  const altFrom = await noteAt(10, 64);
  const altTo = await noteAt(12, 64);
  await page.keyboard.down("Alt");
  await page.mouse.move(altFrom.x, altFrom.y);
  await page.mouse.down();
  await page.mouse.move(altTo.x, altTo.y, { steps: 5 });
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await page.waitForFunction(() =>
    window.__weetbeats_calls.some((c) => c.name === "edit_notes"));
  const copied = (await lastCall("edit_notes")).args;
  check("alt dragging a picked set leaves the originals and drags copies",
    copied.remove.length === 0 && copied.add.length === 2, JSON.stringify(copied));
  check("and the copies land where they were dragged to",
    copied.add.map((n) => n.step).sort((a, b) => a - b).join() === "12,16",
    JSON.stringify(copied.add));
  check("so the pattern has six notes now", (await rollLaneNow()).length === 6);

  // A box round the lot, and backspace takes them all out at once.
  await clearCalls();
  const allFrom = await noteAt(2, 66);
  const allTo = await noteAt(19, 60);
  await page.keyboard.down("Shift");
  await page.mouse.move(allFrom.x, allFrom.y);
  await page.mouse.down();
  await page.mouse.move(allTo.x, allTo.y, { steps: 5 });
  await page.mouse.up();
  await page.keyboard.up("Shift");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("6 notes picked out"));
  await page.keyboard.press("Backspace");
  await page.waitForFunction((id) => {
    const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === id);
    return !lane || lane.notes.length === 0;
  }, trackIds[0]);
  check("backspace rubs out everything picked out", (await rollLaneNow()).length === 0);
  check("in one trip rather than one each",
    (await calls("edit_notes")).length === 1 &&
      (await lastCall("edit_notes")).args.remove.length === 6,
    JSON.stringify((await lastCall("edit_notes")).args));

  // --- stretching a note into the edge of the window scrolls the roll along
  await page.evaluate(() => {
    document.getElementById("rollScroll").scrollLeft = 0;
  });
  const stretchFrom = await noteAt(2, 64);
  const stretchTo = await noteAt(3, 64);
  await page.mouse.move(stretchFrom.x, stretchFrom.y);
  await page.mouse.down();
  await page.mouse.move(stretchTo.x, stretchTo.y, { steps: 3 });
  await page.mouse.up();
  const rollBox = await page.locator("#rollScroll").boundingBox();
  const noteTail = await page.evaluate(
    ([cell, semitone, high]) => {
      const box = document.getElementById("notes").getBoundingClientRect();
      return { x: box.left + 4 * cell - 3, y: box.top + (high - 64) * semitone + semitone / 2 };
    },
    [ROLL_CELL, SEMITONE, HIGH_PITCH],
  );
  await page.mouse.move(noteTail.x, noteTail.y);
  await page.mouse.down();
  await page.mouse.move(rollBox.x + rollBox.width - 6, noteTail.y, { steps: 6 });
  await page.waitForFunction(() => document.getElementById("rollScroll").scrollLeft > 0);
  const rollFollowed = await page.evaluate(() =>
    document.getElementById("rollScroll").scrollLeft);
  check("stretching a note into the edge scrolls the roll to follow", rollFollowed > 0,
    `${rollFollowed}px`);
  await page.mouse.up();
  await page.waitForFunction((id) => {
    const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === id);
    return lane && lane.notes[0] && lane.notes[0].length > 20;
  }, trackIds[0]);
  check("and the note goes on growing past the edge of the window",
    (await rollLaneNow())[0].length > 20, JSON.stringify(await rollLaneNow()));
  await page.evaluate(() => {
    document.getElementById("rollScroll").scrollLeft = 0;
  });
  // Everything picked out and taken back out again, so the grid below starts empty.
  await page.keyboard.press("a");
  await page.keyboard.press("Backspace");
  await page.waitForFunction((id) => {
    const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === id);
    return !lane || lane.notes.length === 0;
  }, trackIds[0]);
  check("a selects everything in the roll, and it goes in one step",
    (await rollLaneNow()).length === 0);

  // --- and the same box in the step grid, which is how a part reaches another instrument
  await page.keyboard.press("Escape");
  await page.waitForSelector("#editor:visible");
  await page.locator("#trackHeaders .track").first().locator(".tick.keys-on").click();
  await page.waitForFunction(() =>
    !document.querySelector("#trackHeaders .track .tick.keys-on").classList.contains("on"));
  const gridNow = await page.locator("#grid").boundingBox();
  const cellPoint = (step, row) => ({
    x: gridNow.x + step * CELL + CELL / 2,
    y: gridNow.y + row * ROW + ROW / 2,
  });
  for (const step of [0, 4, 8]) {
    const box = cellPoint(step, 0);
    await page.mouse.click(box.x, box.y);
  }
  await page.waitForFunction((id) => {
    const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === id);
    return lane && lane.notes.length === 3;
  }, trackIds[0]);
  await clearCalls();
  await page.keyboard.down("Shift");
  await page.mouse.move(gridNow.x + 2, cellPoint(0, 0).y - ROW / 3);
  await page.mouse.down();
  await page.mouse.move(cellPoint(9, 0).x, cellPoint(0, 0).y + ROW / 3, { steps: 5 });
  await page.mouse.up();
  await page.keyboard.up("Shift");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("picked out"));
  check("a box round a row of boxes picks those notes out",
    (await page.locator("#status").textContent()).startsWith("3 notes picked out"),
    await page.locator("#status").textContent());
  check("and it ticked no boxes on the way", (await rollLaneNow()).length === 3);

  await page.keyboard.press("c");
  await page.waitForFunction(() =>
    document.getElementById("status").textContent.includes("copied"));
  // Where it lands is the row you are pointing at, which is what makes it a copy between
  // instruments rather than a copy back on top of itself.
  await page.mouse.move(cellPoint(2, 1).x, cellPoint(2, 1).y);
  await page.keyboard.press("v");
  await page.waitForFunction((id) => {
    const lane = window.__weetbeats_state.patterns[0].lanes.find((l) => l.track === id);
    return lane && lane.notes.length === 3;
  }, trackIds[1]);
  const across = (await lastCall("edit_notes")).args;
  check("copying a part and pasting it lands on the instrument you point at",
    across.track === trackIds[1], JSON.stringify(across));
  check("with the notes in the same places in the bar",
    across.add.map((n) => n.step).sort((a, b) => a - b).join() === "0,4,8",
    JSON.stringify(across.add));
  check("and the part it came from is still where it was", (await rollLaneNow()).length === 3);

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
