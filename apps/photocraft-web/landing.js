(() => {
  "use strict";

  const commerceDeadlineMs = 10_000;
  const buttons = [...document.querySelectorAll("[data-pixelle-start]")];
  const supportButtons = [...document.querySelectorAll("[data-pixelle-support]")];
  let accessInProgress = false;

  function waitForCommerce() {
    if (window.PixelleCommerce) return Promise.resolve(window.PixelleCommerce);
    return new Promise((resolve, reject) => {
      const started = performance.now();
      const poll = setInterval(() => {
        if (window.PixelleCommerce) {
          clearInterval(poll);
          resolve(window.PixelleCommerce);
        } else if (performance.now() - started >= commerceDeadlineMs) {
          clearInterval(poll);
          reject(new Error("Форма входа не загрузилась"));
        }
      }, 50);
    });
  }

  function setBusy(busy) {
    accessInProgress = busy;
    for (const button of buttons) {
      button.disabled = busy;
      button.setAttribute("aria-busy", String(busy));
    }
  }

  function showEntryError(message) {
    const host = document.querySelector("[data-pixelle-entry-error]");
    if (!host) return;
    host.textContent = message;
    host.hidden = false;
  }

  async function startAccess() {
    if (accessInProgress) return;
    setBusy(true);
    try {
      const commerce = await waitForCommerce();
      await commerce.startAccess();
    } catch (error) {
      showEntryError(error?.message || "Не удалось открыть вход. Обновите страницу.");
    } finally {
      setBusy(false);
    }
  }

  for (const button of buttons) button.addEventListener("click", startAccess);
  for (const button of supportButtons) {
    button.addEventListener("click", async () => {
      try { (await waitForCommerce()).showSupport(); }
      catch (error) { showEntryError(error?.message || "Поддержка временно недоступна"); }
    });
  }

  window.addEventListener("pixelle:subscription-active", () => {
    const target = new URL("workspace.html", location.href);
    target.search = "";
    target.hash = "";
    location.assign(target.href);
  });
})();
