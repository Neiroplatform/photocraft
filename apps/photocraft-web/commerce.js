(() => {
  "use strict";
  const API = "/api/v1/pixelle";
  const AUTH = "https://auth.neiro-platform.ru";
  let activeLayer = null;
  let previousFocus = null;
  let returnToPaywall = false;
  const escapeHtml = (value) => String(value).replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]);
  const receiptUrl = (value) => {
    try { const url = new URL(String(value)); return url.protocol === "https:" ? escapeHtml(url.href) : ""; }
    catch { return ""; }
  };

  async function request(path, options = {}) {
    const response = await fetch(`${API}${path}`, {
      credentials: "include",
      ...options,
      headers: { ...(options.body ? { "content-type": "application/json" } : {}), ...(options.headers || {}) }
    });
    const body = await response.json().catch(() => ({}));
    if (!response.ok) {
      const error = new Error(body.message || body.error || `HTTP ${response.status}`);
      error.status = response.status;
      throw error;
    }
    return body;
  }

  function closeLayer() {
    activeLayer?.remove();
    activeLayer = null;
    if (previousFocus instanceof HTMLElement) previousFocus.focus({ preventScroll: true });
    previousFocus = null;
  }

  function layer(content) {
    closeLayer();
    previousFocus = document.activeElement;
    const root = document.createElement("div");
    root.className = "px-commerce-layer";
    root.innerHTML = `<div class="px-commerce-backdrop"></div>`;
    const backdrop = root.firstElementChild;
    backdrop.append(content);
    backdrop.addEventListener("mousedown", (event) => { if (event.target === backdrop) closeLayer(); });
    document.body.append(root);
    activeLayer = root;
    const focusable = () => [...root.querySelectorAll('button:not([disabled]),a[href],input:not([disabled]),select:not([disabled]),textarea:not([disabled]),iframe')];
    root.addEventListener("keydown", (event) => {
      if (event.key === "Escape") { event.preventDefault(); closeLayer(); return; }
      if (event.key !== "Tab") return;
      const items = focusable();
      if (!items.length) return;
      const first = items[0]; const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    });
    queueMicrotask(() => focusable()[0]?.focus());
    return root;
  }

  function closeButton() {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "px-commerce-close";
    button.setAttribute("aria-label", "Закрыть");
    button.textContent = "×";
    button.addEventListener("click", closeLayer);
    return button;
  }

  function showAuth(nextPaywall = false) {
    returnToPaywall = nextPaywall;
    const box = document.createElement("div");
    box.className = "px-commerce-auth";
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-label", "Вход в Pixelle");
    box.append(closeButton());
    const frame = document.createElement("iframe");
    frame.title = "Вход в Pixelle";
    frame.src = `${AUTH}/auth/login?origin=${encodeURIComponent(location.origin)}`;
    box.append(frame);
    layer(box);
  }

  window.addEventListener("message", async (event) => {
    if (event.origin !== new URL(AUTH).origin || event.data?.type !== "auth_exchange" || typeof event.data.code !== "string") return;
    try {
      await request("/auth/exchange", { method: "POST", body: JSON.stringify({ code: event.data.code }) });
      const next = returnToPaywall;
      closeLayer();
      if (next) showPaywall();
    } catch {
      frameError("Не удалось завершить вход. Закройте форму и попробуйте ещё раз.");
    }
  });

  function frameError(message) {
    const host = activeLayer?.querySelector(".paywall-modal, .px-commerce-card, .px-commerce-auth");
    if (!host) return;
    let item = host.querySelector(".px-commerce-error");
    if (!item) { item = document.createElement("p"); item.className = "px-commerce-error"; host.append(item); }
    item.textContent = message;
  }

  async function loadWidget() {
    if (window.cp?.CloudPayments) return;
    await new Promise((resolve, reject) => {
      const old = document.querySelector('script[src*="widget.cloudpayments.ru/bundles/cloudpayments"]');
      if (old) {
        const started = Date.now();
        const poll = setInterval(() => {
          if (window.cp?.CloudPayments) { clearInterval(poll); resolve(); }
          else if (Date.now() - started > 10000) { clearInterval(poll); reject(new Error("Платёжная форма недоступна")); }
        }, 100);
        return;
      }
      const script = document.createElement("script");
      script.src = "https://widget.cloudpayments.ru/bundles/cloudpayments.js";
      script.crossOrigin = "anonymous";
      script.onload = resolve;
      script.onerror = () => reject(new Error("Платёжная форма недоступна"));
      document.head.append(script);
    });
  }

  async function checkout(button, consent, recurring) {
    if (!consent.checked || !recurring.checked) return;
    button.disabled = true;
    button.textContent = "Открываем оплату…";
    try {
      const order = await request("/subscription/create", {
        method: "POST",
        body: JSON.stringify({ acceptTerms: true, acceptPrivacy: true, acceptRecurring: true })
      });
      await loadWidget();
      const widget = new window.cp.CloudPayments();
      const result = await widget.start({
        publicTerminalId: order.publicId,
        description: order.description,
        paymentSchema: "Single",
        currency: order.currency,
        culture: "ru-RU",
        amount: order.amount,
        externalId: order.orderId,
        receiptEmail: order.email,
        tokenize: true,
        userInfo: { accountId: order.accountId, email: order.email },
        receipt: order.receipt
      });
      if (result?.status !== "success") throw new Error("Платёж не был подтверждён");
      for (let attempt = 0; attempt < 10; attempt += 1) {
        try {
          await request(`/subscription/finalize/${encodeURIComponent(order.orderId)}`, { method: "POST" });
          closeLayer();
          window.dispatchEvent(new CustomEvent("pixelle:subscription-active"));
          return;
        } catch (error) {
          if (error.status !== 409 || attempt === 9) throw error;
          await new Promise((resolve) => setTimeout(resolve, 1500));
        }
      }
    } catch (error) {
      if (error.status === 401) { showAuth(true); return; }
      frameError(error.message || "Не удалось открыть оплату. Попробуйте позже.");
      button.disabled = false;
      button.textContent = "Оплатить картой";
    }
  }

  function showPaywall() {
    const card = document.createElement("section");
    card.className = "paywall-modal ym-hide-content";
    card.setAttribute("role", "dialog");
    card.setAttribute("aria-modal", "true");
    card.setAttribute("aria-labelledby", "pixelle-paywall-title");
    card.innerHTML = `
      <header class="paywall-header">
        <button class="paywall-close" type="button" aria-label="Закрыть">×</button>
        <h2 id="pixelle-paywall-title">Получите Pixelle Pro за 9 ₽</h2>
        <div class="paywall-price-row"><span class="paywall-old-price">1 617 ₽</span><span class="paywall-discount">Первые 3 дня</span></div>
      </header>
      <div class="paywall-body">
        <ul class="paywall-benefits">
          <li>10 локальных проектов в пробном периоде</li>
          <li>100 проектов с локальной обработкой после продления</li>
          <li>До 300 ₽ по фактическому расходу токенов на облачные генерации</li>
          <li>Редактор, объектное выделение и локальные инструменты без отправки пикселей</li>
        </ul>
        <div class="paywall-consent">
          <label><input id="pixelle-recurring" type="checkbox"><span>Согласен(-на) на 9 ₽ за 3 пробных дня, затем 1 617 ₽ каждые 5 дней. После трёх неудачных попыток повтор — раз в месяц; успешная оплата возвращает периодичность 5 дней.</span></label>
          <label><input id="pixelle-terms" type="checkbox"><span>Принимаю <a href="pixelle-offer.html" target="_blank" rel="noreferrer">оферту</a>, <a href="pixelle-rules.html" target="_blank" rel="noreferrer">правила сервиса</a> и <a href="pixelle-privacy.html" target="_blank" rel="noreferrer">политику конфиденциальности</a>.</span></label>
        </div>
      </div>
      <div class="paywall-actions">
        <button class="paywall-payment paywall-payment-card" type="button" disabled><span class="paywall-card-icon" aria-hidden="true"></span>Оплата картой</button>
        <p class="paywall-note">Будущие платежи можно отменить в профиле Pixelle Pro; доступ сохранится до конца оплаченного периода.</p>
      </div>`;
    card.querySelector(".paywall-close").addEventListener("click", closeLayer);
    const consent = card.querySelector("#pixelle-terms");
    const recurring = card.querySelector("#pixelle-recurring");
    const pay = card.querySelector(".paywall-payment-card");
    const sync = () => { pay.disabled = !(consent.checked && recurring.checked); };
    consent.addEventListener("change", sync);
    recurring.addEventListener("change", sync);
    pay.addEventListener("click", () => checkout(pay, consent, recurring));
    layer(card);
    request("/subscription/status").then((status) => {
      if (status.trialAvailable !== false) return;
      const title = card.querySelector("#pixelle-paywall-title");
      const price = card.querySelector(".paywall-price-row");
      const recurringText = card.querySelector("#pixelle-recurring")?.nextElementSibling;
      if (title) title.textContent = "Возобновите Pixelle Pro";
      if (price) price.innerHTML = "<strong>1 617 ₽</strong><span>на 5 дней</span>";
      if (recurringText) recurringText.textContent = "Согласен(-на) на списание 1 617 ₽ за 5 дней, затем 1 617 ₽ каждые 5 дней. После трёх неудачных попыток повтор — раз в месяц; успешная оплата возвращает периодичность 5 дней.";
    }).catch((error) => { if (error.status === 401) showAuth(true); });
  }

  function showSupport() {
    const card = document.createElement("section");
    card.className = "px-commerce-card";
    card.setAttribute("role", "dialog");
    card.setAttribute("aria-modal", "true");
    card.innerHTML = `<div class="px-commerce-stripe"></div><div class="px-commerce-head"><h2>Поддержка Pixelle</h2><p>Сообщение появится в admin.neiro-platform.ru и будет связано с вашим аккаунтом.</p></div><div class="px-commerce-body"><form class="px-support-form"><select name="category"><option value="other">Вопрос по редактору</option><option value="subscription">Подписка и оплата</option><option value="refund">Возврат</option><option value="technical">Техническая проблема</option></select><input name="subject" maxlength="255" placeholder="Тема" required><textarea name="body" maxlength="8000" placeholder="Опишите проблему" required></textarea><button class="px-commerce-primary" type="submit">Отправить</button></form></div>`;
    card.append(closeButton());
    const form = card.querySelector("form");
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const button = form.querySelector("button");
      button.disabled = true;
      try {
        const data = Object.fromEntries(new FormData(form));
        const ticket = await request("/support/tickets", { method: "POST", body: JSON.stringify(data) });
        form.replaceWith(Object.assign(document.createElement("div"), { className: "px-support-success", textContent: `Обращение ${ticket.id} создано. Ответ придёт на email вашего аккаунта.` }));
      } catch (error) {
        if (error.status === 401) { showAuth(false); return; }
        frameError(error.message || "Не удалось отправить обращение.");
        button.disabled = false;
      }
    });
    layer(card);
  }

  function accountCard(status, receipts) {
    const card = document.createElement("section");
    card.className = "px-commerce-card";
    card.setAttribute("role", "dialog");
    card.setAttribute("aria-modal", "true");
    const project = status.localProjects || { used: 0, limit: 0 };
    const cloud = status.cloudBudgetKopecks || { used: 0, limit: 0 };
    const receiptItems = receipts.length
      ? receipts.map((receipt) => { const url = receiptUrl(receipt.ofd_receipt_url); return `<li>${new Date(receipt.created_at).toLocaleDateString("ru-RU")} · ${(Number(receipt.amount_kopecks || 0) / 100).toLocaleString("ru-RU")} ₽${url ? ` · <a href="${url}" target="_blank" rel="noreferrer">Открыть чек</a>` : " · чек обрабатывается"}</li>`; }).join("")
      : "<li>Чеков пока нет</li>";
    const cancellation = status.cancelAtPeriodEnd
      ? `<p class="px-commerce-note">Будущие списания отменены. Доступ сохранится до ${status.periodEnd ? new Date(status.periodEnd).toLocaleString("ru-RU") : "конца оплаченного периода"}.</p>`
      : `<button class="px-commerce-primary" data-action="cancel" type="button">Отменить будущие списания</button><p class="px-commerce-note">Доступ сохранится до конца уже оплаченного периода.</p>`;
    card.innerHTML = `<div class="px-commerce-stripe"></div><div class="px-commerce-head"><h2>Pixelle Pro</h2><p>Статус: ${status.status === "trial" ? "пробный период" : "активна"}</p></div><div class="px-commerce-body"><ul class="px-commerce-benefits"><li>Локальные проекты: ${project.used} из ${project.limit}</li><li>Облачный ИИ: ${(cloud.used / 100).toLocaleString("ru-RU")} ₽ из ${(cloud.limit / 100).toLocaleString("ru-RU")} ₽</li><li>Период до: ${status.periodEnd ? new Date(status.periodEnd).toLocaleString("ru-RU") : "—"}</li></ul><h3>Фискальные чеки</h3><ul>${receiptItems}</ul>${cancellation}</div>`;
    card.append(closeButton());
    card.querySelector('[data-action="cancel"]')?.addEventListener("click", async (event) => {
      const button = event.currentTarget;
      button.disabled = true;
      try { await request("/subscription/cancel", { method: "POST" }); closeLayer(); }
      catch (error) { frameError(error.message || "Не удалось отменить подписку"); button.disabled = false; }
    });
    return card;
  }

  async function showAccount() {
    try {
      const status = await request("/subscription/status");
      if (!status || status.status === "none") { showPaywall(); return; }
      const receipts = await request("/subscription/receipts").catch(() => []);
      layer(accountCard(status, Array.isArray(receipts) ? receipts : []));
    } catch (error) {
      if (error.status === 401) { showAuth(false); return; }
      frameError(error.message || "Личный кабинет временно недоступен");
    }
  }

  function addButtons() {
    if (document.getElementById("pixelle-pro-button")) return;
    const pro = document.createElement("button");
    pro.id = "pixelle-pro-button";
    pro.className = "px-commerce-fab";
    pro.type = "button";
    pro.textContent = "Pixelle Pro";
    pro.addEventListener("click", showAccount);
    const support = document.createElement("button");
    support.id = "pixelle-support-button";
    support.className = "px-commerce-fab";
    support.type = "button";
    support.textContent = "?";
    support.setAttribute("aria-label", "Поддержка Pixelle");
    support.addEventListener("click", showSupport);
    document.body.append(pro, support);
  }

  window.PixelleCommerce = { showPaywall, showSupport, showAuth, showAccount };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", addButtons); else addButtons();
})();
