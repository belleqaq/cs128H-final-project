// Brownshock — browser front-end.
//
// Responsibilities:
//   1. Build a fixed 80x22 grid of DOM spans.
//   2. Open a WebSocket back to the local server, render snapshots as they
//      arrive.
//   3. Translate the keyboard into ClientMessage JSON:
//        A/D -> {type:"input",left,right}  (only on state change; SOCD is
//               resolved server-side so we just mirror what's held)
//        W/S -> {type:"stair",dy:-1|1}     (rising edge only)
//        R   -> {type:"restart"}
//
// No frameworks: vanilla JS, so the embedded bundle stays tiny.

(() => {
  const TILE_GLYPH = {
    floor: ".",
    wall: "#",
    goal: "T",
    stair_up: "^",
    stair_down: "v",
  };
  const TILE_CLASS = {
    floor: "floor",
    wall: "wall",
    goal: "goal",
    stair_up: "stair",
    stair_down: "stair",
  };

  const gridEl = document.getElementById("grid");
  const hudTop = document.getElementById("hud-top");
  const hudStatus = document.getElementById("status");

  // The first snapshot tells us width/height; build the grid then.
  let gridBuilt = false;
  let cells = []; // flat array, indexed [y * width + x]
  let width = 0;
  let height = 0;

  function buildGrid(w, h) {
    gridEl.innerHTML = "";
    cells = new Array(w * h);
    for (let y = 0; y < h; y++) {
      const row = document.createElement("div");
      row.className = "row";
      for (let x = 0; x < w; x++) {
        const cell = document.createElement("span");
        cell.className = "cell floor";
        cell.textContent = ".";
        row.appendChild(cell);
        cells[y * w + x] = cell;
      }
      gridEl.appendChild(row);
    }
    width = w;
    height = h;
    gridBuilt = true;
  }

  function render(snap) {
    if (!gridBuilt || snap.width !== width || snap.height !== height) {
      buildGrid(snap.width, snap.height);
    }
    // Draw the map. `snap.map` is a flat array of tile-kind strings.
    for (let i = 0; i < snap.map.length; i++) {
      const kind = snap.map[i];
      const cell = cells[i];
      const cls = "cell " + (TILE_CLASS[kind] ?? "floor");
      if (cell.className !== cls) cell.className = cls;
      const ch = TILE_GLYPH[kind] ?? " ";
      if (cell.textContent !== ch) cell.textContent = ch;
    }
    // Overdraw NPCs.
    for (const npc of snap.npcs) {
      const [x, y] = npc.pos;
      const cell = cells[y * width + x];
      if (cell) {
        cell.className = "cell npc";
        cell.textContent = npc.symbol;
      }
    }
    // Overdraw the player last so it wins any cell conflict.
    {
      const [x, y] = snap.player;
      const cell = cells[y * width + x];
      if (cell) {
        cell.className = "cell player";
        cell.textContent = "@";
      }
    }

    // Phase HUD.
    if (snap.phase === "win") {
      hudTop.textContent = "YOU WIN! Press R to restart.";
      hudTop.className = "hud win";
    } else if (snap.phase === "lose") {
      hudTop.textContent = "YOU LOSE. Press R to restart.";
      hudTop.className = "hud lose";
    } else {
      hudTop.textContent = "";
      hudTop.className = "hud";
    }
  }

  // -------------------------------------------------------------------------
  // WebSocket
  // -------------------------------------------------------------------------

  let ws = null;
  let wsReady = false;

  function connect() {
    const url = (location.protocol === "https:" ? "wss://" : "ws://") +
      location.host + "/ws";
    ws = new WebSocket(url);
    ws.addEventListener("open", () => {
      wsReady = true;
      hudStatus.textContent = "connected";
      hudStatus.className = "hud good";
      // Re-sync any currently held keys now that we can talk.
      sendInputIfChanged(true);
    });
    ws.addEventListener("message", (ev) => {
      try {
        render(JSON.parse(ev.data));
      } catch (e) {
        console.error("bad snapshot", e);
      }
    });
    ws.addEventListener("close", () => {
      wsReady = false;
      hudStatus.textContent = "disconnected — reconnecting…";
      hudStatus.className = "hud warn";
      setTimeout(connect, 500);
    });
    ws.addEventListener("error", () => {
      // error is always followed by close; let close handle reconnect.
    });
  }

  function send(obj) {
    if (wsReady && ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(obj));
    }
  }

  // -------------------------------------------------------------------------
  // Keyboard
  // -------------------------------------------------------------------------

  const held = { left: false, right: false, up: false, down: false };
  let lastSent = { left: false, right: false, up: false, down: false };

  function sendInputIfChanged(force = false) {
    if (force || held.left !== lastSent.left || held.right !== lastSent.right || held.up !== lastSent.up || held.down !== lastSent.down) {
      lastSent = { ...held };
      send({ type: "input", left: held.left, right: held.right, up: held.up, down: held.down });
    }
  }

  function isMovementKey(k) {
    return k === "a" || k === "d" || k === "arrowleft" || k === "arrowright" || k === "w" || k === "s" || k === "arrowup" || k === "arrowdown";
  }

  window.addEventListener("keydown", (e) => {
    // Let browser shortcuts (Ctrl+R, Cmd+W, etc.) work normally.
    if (e.ctrlKey || e.metaKey || e.altKey) return;

    const k = e.key.toLowerCase();

    if (isMovementKey(k)) {
      e.preventDefault();
      if (e.repeat) return; // holding is handled by held-flag, not repeat
      if (k === "a" || k === "arrowleft") held.left = true;
      if (k === "d" || k === "arrowright") held.right = true;
      if (k === "w" || k === "arrowup") held.up = true;
      if (k === "s" || k === "arrowdown") held.down = true;
      sendInputIfChanged();
      return;
    }

    if (k === "q") {
      e.preventDefault();
      if (!e.repeat) send({ type: "lose" });
      return;
    }

    if (k === "r") {
      e.preventDefault();
      send({ type: "restart" });
      return;
    }
  });

  window.addEventListener("keyup", (e) => {
    const k = e.key.toLowerCase();
    if (isMovementKey(k)) {
      e.preventDefault();
      if (k === "a" || k === "arrowleft")  held.left  = false;
      if (k === "d" || k === "arrowright") held.right = false;
      if (k === "w" || k === "arrowup")    held.up    = false;
      if (k === "s" || k === "arrowdown")  held.down  = false;
      sendInputIfChanged();
    }
  });

  // Losing focus or leaving the tab should release all held keys, otherwise
  // the server would think we're still holding after we come back.
  window.addEventListener("blur", () => {
    held.left = false;
    held.right = false;
    sendInputIfChanged();
  });

  connect();
})();
