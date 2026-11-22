const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");
const { test } = require("node:test");
const vm = require("node:vm");

// Execute the shipped shell, including its landing redirect, without a payment
// provider or real account. A dialog request is a failure in the paid paths.
function shell(respond) {
  const requests = [];
  const navigations = [];
  const listeners = new Map();
  const location = {
    href: "https://fixture.invalid/releases/exact/patchy.html",
    origin: "https://fixture.invalid",
    assign: (url) => navigations.push(url),
  };
  const window = {
    ym() {},
    addEventListener(name, callback) {
      const callbacks = listeners.get(name) || [];
      callbacks.push(callback);
      listeners.set(name, callbacks);
    },
    dispatchEvent(event) {
      for (const callback of listeners.get(event.type) || []) callback(event);
    },
  };
  const document = {
    readyState: "complete",
    body: { dataset: { pixelleSurface: "landing" } },
    querySelectorAll: () => [],
    querySelector: () => null,
    createElement: () => { throw new Error("Unexpected authentication or payment dialog"); },
  };
  const context = vm.createContext({
    window, document, location, URL,
    history: { replaceState() {} },
    sessionStorage: { getItem: () => null },
    CustomEvent: class { constructor(type) { this.type = type; } },
    HTMLElement: class {},
    setTimeout: () => 0, setInterval: () => 0, clearInterval() {},
    queueMicrotask() {}, performance: { now: () => 0 },
    async fetch(url, options) {
      const path = url.replace("/api/v1/pixelle", "");
      requests.push({ path, options });
      const { status = 200, body = {} } = await respond(path, options);
      return { ok: status >= 200 && status < 300, status, json: async () => body };
    },
  });
  for (const filename of ["commerce.js", "landing.js"]) {
    let source = readFileSync(join(__dirname, "..", filename), "utf8");
    if (filename === "commerce.js") {
      // Expose the actual request closure only inside this test VM. Production
      // keeps it private; no authentication or payment behavior is replaced.
      assert.equal(source.split("window.PixelleCommerce = {").length, 2);
      source = source.replace("window.PixelleCommerce = {", "window.PixelleCommerce = { request,");
    }
    vm.runInContext(source, context, { filename });
  }
  return {
    commerce: window.PixelleCommerce, requests, navigations,
    message: (event) => Promise.all((listeners.get("message") || []).map((callback) => callback(event))),
  };
}

test("a returning paid trial refreshes the session and enters its immutable workspace", async () => {
  let refreshed = false;
  const fixture = shell((path) => {
    if (path === "/auth/refresh") { refreshed = true; return { body: { ok: true } }; }
    return refreshed ? { body: { status: "trial" } } : { status: 401 };
  });
  await fixture.commerce.startAccess();
  assert.deepEqual(fixture.requests.map(({ path }) => path), [
    "/subscription/status", "/auth/refresh", "/subscription/status",
  ]);
  assert.deepEqual(fixture.navigations, ["https://fixture.invalid/releases/exact/workspace.html"]);
  assert.ok(fixture.requests.every(({ options }) => options.credentials === "include"));
  assert.equal(fixture.requests[1].options.method, "POST");
});

test("concurrent expired requests share one rotating refresh", async () => {
  let refreshed = false;
  const fixture = shell(async (path) => {
    if (path === "/auth/refresh") {
      await new Promise((resolve) => setImmediate(resolve));
      refreshed = true;
      return { body: { ok: true } };
    }
    return refreshed ? { body: { status: "trial" } } : { status: 401 };
  });
  await Promise.all([fixture.commerce.startAccess(), fixture.commerce.startAccess()]);
  assert.equal(fixture.requests.filter(({ path }) => path === "/auth/refresh").length, 1);
  assert.equal(fixture.navigations.length, 2);
});

test("a delayed stale 401 retries the refreshed session without rotating twice", async () => {
  let statuses = 0;
  let releaseStale;
  const stale = new Promise((resolve) => { releaseStale = resolve; });
  const fixture = shell(async (path) => {
    if (path === "/auth/refresh") return { body: { ok: true } };
    statuses += 1;
    if (statuses === 1) { await stale; return { status: 401 }; }
    return statuses === 2 ? { status: 401 } : { body: { status: "trial" } };
  });
  const delayed = fixture.commerce.startAccess();
  await fixture.commerce.startAccess();
  releaseStale();
  await delayed;
  assert.equal(fixture.requests.filter(({ path }) => path === "/auth/refresh").length, 1);
  assert.equal(fixture.navigations.length, 2);
});

test("temporary refresh failure is surfaced and never opens another checkout", async () => {
  const fixture = shell((path) => path === "/auth/refresh"
    ? { status: 503, body: { message: "Session service temporarily unavailable" } }
    : { status: 401 });
  await assert.rejects(fixture.commerce.startAccess(), { message: "Session service temporarily unavailable", status: 503 });
  assert.equal(fixture.requests.length, 2);
  assert.equal(fixture.navigations.length, 0);
});

test("an already valid trial opens without any refresh or payment request", async () => {
  const fixture = shell(() => ({ body: { status: "trial" } }));
  await fixture.commerce.startAccess();
  assert.deepEqual(fixture.requests.map(({ path }) => path), ["/subscription/status"]);
  assert.equal(fixture.navigations.length, 1);
});

test("an expired refresh cookie enters login after one bounded refresh", async () => {
  const fixture = shell(() => ({ status: 401 }));
  await assert.rejects(fixture.commerce.startAccess(), /Unexpected authentication or payment dialog/);
  assert.deepEqual(fixture.requests.map(({ path }) => path), ["/subscription/status", "/auth/refresh"]);
  assert.equal(fixture.navigations.length, 0);
});

test("a rejected retry enters login without looping or granting access", async () => {
  const fixture = shell((path) => path === "/auth/refresh" ? { body: { ok: true } } : { status: 401 });
  await assert.rejects(fixture.commerce.startAccess(), /Unexpected authentication or payment dialog/);
  assert.deepEqual(fixture.requests.map(({ path }) => path), [
    "/subscription/status", "/auth/refresh", "/subscription/status",
  ]);
  assert.equal(fixture.navigations.length, 0);
});

test("network failure and non-auth denials never refresh or grant access", async () => {
  for (const status of [403, 500, 503]) {
    const fixture = shell(() => ({ status, body: { message: "Service unavailable" } }));
    await assert.rejects(fixture.commerce.startAccess(), { status });
    assert.equal(fixture.requests.length, 1);
    assert.equal(fixture.navigations.length, 0);
  }
  const fixture = shell(() => { throw new Error("Network unavailable"); });
  await assert.rejects(fixture.commerce.startAccess(), /Network unavailable/);
  assert.equal(fixture.requests.length, 1);
  assert.equal(fixture.navigations.length, 0);
});

test("one-use authorization codes are never refreshed or replayed", async () => {
  const fixture = shell(() => ({ status: 401 }));
  await fixture.message({ origin: "https://auth.neiro-platform.ru", data: { type: "auth_exchange", code: "a".repeat(64) } });
  assert.deepEqual(fixture.requests.map(({ path }) => path), ["/auth/exchange"]);
  assert.equal(fixture.navigations.length, 0);
});

test("checkout finalization after 401 preserves the order and body without creating another charge", async () => {
  let refreshed = false;
  const orderPath = "/subscription/finalize/pixelle-trial-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
  const fixture = shell((path) => {
    if (path === "/auth/refresh") { refreshed = true; return { body: { ok: true } }; }
    assert.equal(path, orderPath);
    return refreshed ? { body: { status: "trial" } } : { status: 401 };
  });
  const options = { method: "POST", body: "{}", headers: { "x-request-id": "exact-finalization" } };
  assert.equal((await fixture.commerce.request(orderPath, options)).status, "trial");
  assert.deepEqual(fixture.requests.map(({ path }) => path), [orderPath, "/auth/refresh", orderPath]);
  for (const request of [fixture.requests[0], fixture.requests[2]]) {
    assert.equal(request.options.method, "POST");
    assert.equal(request.options.body, "{}");
    assert.equal(request.options.headers["x-request-id"], "exact-finalization");
    assert.equal(request.options.headers["content-type"], "application/json");
  }
});
