import { test, expect } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import { createServer } from 'node:http';

// Synthetic fixtures only. Every WeChat API request is intercepted; nothing is sent to WeChat.
const TOKEN = 'we-bot-browser-fixture-token-000000000000';
const A = 'wa_111111111111111111111111111a7c92';
const B = 'wa_222222222222222222222222224b9e10';
const message = (id, text, direction = 'incoming', account = A, minutesAgo = 0) => ({
  id, account_id: account, text, direction, created_at_ms: Date.now() - minutesAgo * 60000,
});

async function fixture(page, options = {}) {
  const model = {
    accounts: [{ id: A, display_name: '微信账户 · 1a7c92', state: 'ready', monitor_running: true }],
    histories: new Map([[A, [
      message('in:1', '你好，先发一条消息，让通知通道准备好。', 'incoming', A, 5),
      message('out:1', '这是一条来自 we-bot 的测试消息。\n收到后，请在微信里回复。', 'outgoing', A, 4),
      message('in:2', '收到了，微信这边可以正常回复。', 'incoming', A, 3),
    ]]]),
    sent: [], historySaved: true, authMini: false, ...options,
  };
  await page.route('**/auth/config', (route) => route.fulfill({
    contentType: 'application/json', body: JSON.stringify({ mode: model.authMini ? 'auth_mini' : 'api_token' }),
  }));
  await page.route('**/wechat/accounts**', async (route) => {
    const request = route.request();
    const reply = (body, status = 200) => route.fulfill({ status, contentType: 'application/json', headers: { 'Cache-Control': 'no-store' }, body: JSON.stringify(body) });
    if ((!model.authMini && request.headers().authorization !== `Bearer ${TOKEN}`) || model.expired) {
      return reply({ error: { code: model.authMini ? 'login_required' : 'unauthorized' } }, 401);
    }
    if (model.authMini) {
      expect(request.headers().authorization).toBeUndefined();
      if (model.denied) return reply({ error: { code: 'access_denied' } }, 403);
      if (model.authUnavailable) return reply({ error: { code: 'auth_unavailable' } }, 503);
    }
    const path = new URL(request.url()).pathname;
    if (path === '/wechat/accounts') {
      return reply({ accounts: model.accounts.map((account) => ({ ...account, last_message: model.histories.get(account.id)?.at(-1) || null })) });
    }
    const id = path.split('/')[3];
    if (!model.accounts.some((account) => account.id === id)) return reply({ error: { code: 'account_not_found' } }, 404);
    if (request.method() === 'GET') {
      if (model.historyError) return reply({ error: { code: 'unavailable' } }, 503);
      const snapshot = structuredClone(model.histories.get(id) || []);
      if (model.delayHistory === id) {
        model.delayHistory = null;
        await new Promise((resolve) => { model.releaseHistory = resolve; });
      }
      return reply({ account_id: id, messages: snapshot, limit: 200 });
    }
    const input = request.postDataJSON();
    if (model.authMini) {
      const headers = await request.allHeaders();
      model.lastSendOrigin = headers.origin;
      model.lastSendCookie = headers.cookie;
      if (headers.origin !== new URL(request.url()).origin) return reply({ error: { code: 'invalid_origin' } }, 403);
    }
    model.sent.push({ id, text: input.text });
    if (model.holdSend) await new Promise((resolve) => { model.releaseSend = resolve; });
    if (model.sendError) return reply({ error: { code: model.sendError } }, model.sendError === 'rate_limited' ? 429 : 502);
    const sent = message(`out:${model.sent.length + 10}`, input.text, 'outgoing', id);
    const history = model.histories.get(id) || [];
    history.push(sent);
    model.histories.set(id, history);
    return reply({ message: sent, history_saved: model.historySaved });
  });
  await page.goto('/');
  return model;
}

async function connect(page) {
  await page.getByLabel('API Token').fill(TOKEN);
  await page.getByRole('button', { name: '连接服务器' }).click();
  await expect(page.locator('#workspace')).toBeVisible();
}

async function open(page, id = A) {
  await page.locator(`[data-account-id="${id}"]`).click();
  await expect(page.locator('#messages .message').first()).toBeVisible();
}

test('desktop and mobile account/conversation layouts', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await fixture(page);
  await connect(page);
  await open(page);
  await mkdir('.impeccable/review', { recursive: true });
  await expect(page.locator('#chat-title')).toHaveText('微信账户 · 1a7c92');
  await page.screenshot({ path: '.impeccable/review/desktop.png', fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole('button', { name: '返回账户列表' })).toBeVisible();
  await expect(page.locator('#send-button')).toBeVisible();
  await page.screenshot({ path: '.impeccable/review/mobile.png', fullPage: true });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.getByRole('button', { name: '返回账户列表' }).click();
  await expect(page.locator(`[data-account-id="${A}"]`)).toBeVisible();
  await page.screenshot({ path: '.impeccable/review/mobile-accounts.png', fullPage: true });
  await page.setViewportSize({ width: 320, height: 568 });
  await open(page);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await expect(page.locator('#send-button')).toBeInViewport();
  expect(errors).toEqual([]);
});

test('authentication, targeted send, polling replies, and Chinese composition', async ({ page }) => {
  const model = await fixture(page);
  await page.getByLabel('API Token').fill('an-incorrect-token-that-is-long-enough');
  await page.getByRole('button', { name: '连接服务器' }).click();
  await expect(page.locator('#connect-error')).toContainText('API Token 不正确');
  await expect(page.locator('#workspace')).toBeHidden();
  await connect(page);
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
  await expect(page.getByLabel('API Token')).toHaveValue('');
  await open(page);
  await page.getByRole('button', { name: '查看账户映射' }).click();
  await expect(page.locator('#mapping-account')).toHaveText(A);
  await page.getByRole('button', { name: '填入测试消息' }).click();
  expect(model.sent).toHaveLength(0);
  await page.locator('#send-button').click();
  await expect(page.locator('#message-input')).toHaveValue('');
  expect(model.sent).toEqual([{ id: A, text: '这是一条来自 we-bot 的测试消息。收到后，请在微信里回复。' }]);
  model.histories.get(A).push(message('in:3', '新的微信回信，自动显示。'));
  await expect(page.locator('.message-bubble').filter({ hasText: '新的微信回信，自动显示。' })).toBeVisible({ timeout: 8000 });
  await expect(page.locator('#message-feed')).toHaveAttribute('aria-live', 'off');
  await expect(page.locator('#conversation-announcer')).toHaveText('收到 1 条微信新消息。新的微信回信，自动显示。');
  await page.locator('#message-input').fill('中文输入法');
  await page.locator('#message-input').dispatchEvent('compositionstart');
  await page.locator('#message-input').press('Enter');
  expect(model.sent).toHaveLength(1);
  await page.locator('#message-input').dispatchEvent('compositionend');
  await page.locator('#message-input').press('Shift+Enter');
  expect(model.sent).toHaveLength(1);
  await page.locator('#message-input').press('Enter');
  await expect.poll(() => model.sent.length).toBe(2);
  await expect(page.locator('#message-input')).toHaveValue('');
  await page.locator('#message-input').fill('微'.repeat(4001));
  await expect(page.locator('#send-button')).toBeDisabled();
  await expect(page.locator('#message-input')).toHaveAttribute('aria-invalid', 'true');
});

test('failed sends preserve drafts and do not retry automatically', async ({ page }) => {
  const model = await fixture(page, { sendError: 'provider_unavailable' });
  await connect(page);
  await open(page);
  await page.locator('#message-input').fill('应保留的草稿');
  await page.locator('#send-button').click();
  await expect(page.locator('#send-error')).toContainText('未能确认发送结果');
  await expect(page.locator('#message-input')).toHaveValue('应保留的草稿');
  await expect(page.locator('.message-bubble').filter({ hasText: '应保留的草稿' })).toHaveCount(0);
  await page.locator('#refresh').click();
  expect(model.sent).toHaveLength(1);
  model.sendError = 'rate_limited';
  await page.locator('#send-button').click();
  await expect(page.locator('#send-error')).toContainText('发送过于频繁');
  model.sendError = null;
  model.historySaved = false;
  await page.locator('#send-button').click();
  await expect(page.locator('#send-error')).toContainText('消息已提交微信，但会话记录未能保存');
  await expect(page.locator('#message-input')).toHaveValue('');
  await expect(page.locator('.message-bubble').filter({ hasText: '应保留的草稿' })).toHaveCount(1);
});

test('multiple account drafts and late responses never mix conversations', async ({ page }) => {
  const model = await fixture(page);
  model.accounts.push({ id: B, display_name: '微信账户 · 4b9e10', state: 'ready', monitor_running: true });
  model.histories.set(B, [message('in:b', '第二个账户的独立消息', 'incoming', B)]);
  await connect(page);
  await open(page);
  await page.locator('#message-input').fill('只给第一个账户的草稿');
  await open(page, B);
  await expect(page.locator('#message-input')).toHaveValue('');
  await open(page, A);
  await expect(page.locator('#message-input')).toHaveValue('只给第一个账户的草稿');
  model.holdSend = true;
  await page.locator('#send-button').click();
  await expect.poll(() => !!model.releaseSend).toBe(true);
  await open(page, B);
  model.releaseSend();
  await expect(page.locator('.message-bubble')).toHaveText(['第二个账户的独立消息']);
  await open(page, A);
  await expect(page.locator('.message-bubble').last()).toHaveText('只给第一个账户的草稿');
  await open(page, B);
  model.delayHistory = A;
  await page.locator(`[data-account-id="${A}"]`).click();
  await expect.poll(() => !!model.releaseHistory).toBe(true);
  await open(page, B);
  model.releaseHistory();
  await expect(page.locator('#chat-title')).toHaveText('微信账户 · 4b9e10');
  await expect(page.locator('.message-bubble')).toHaveText(['第二个账户的独立消息']);
  expect(model.sent).toEqual([{ id: A, text: '只给第一个账户的草稿' }]);
});

test('new replies preserve history scroll position and render text without HTML', async ({ page }) => {
  const model = await fixture(page);
  model.histories.set(A, Array.from({ length: 40 }, (_, index) => message(`in:${index}`, `第 ${index + 1} 条历史消息。保持这段对话的阅读位置。`)));
  await connect(page);
  await open(page);
  const top = await page.locator('#message-feed').evaluate((feed) => { feed.scrollTop = 120; return feed.scrollTop; });
  model.histories.get(A).push(message('in:html', '<img src=x onerror="window.injected=true"> 新回复'));
  await expect(page.getByRole('button', { name: '查看新消息' })).toBeVisible({ timeout: 8000 });
  expect(await page.locator('#message-feed').evaluate((feed) => feed.scrollTop)).toBeCloseTo(top, 0);
  await expect(page.locator('.message-bubble img')).toHaveCount(0);
  expect(await page.evaluate(() => window.injected)).toBeUndefined();
  await page.getByRole('button', { name: '查看新消息' }).click();
  await expect(page.getByRole('button', { name: '查看新消息' })).toBeHidden();
  await expect(page.locator('.message-bubble').last()).toBeInViewport();
});

test('activation, expired binding, read failure, and expired API credentials are distinct', async ({ page }) => {
  const model = await fixture(page);
  model.accounts[0].state = 'waiting_for_message';
  await connect(page);
  await open(page);
  await expect(page.locator('#chat-state-notice')).toContainText('请先在微信中');
  await expect(page.locator('#message-input')).toBeDisabled();
  model.accounts[0].state = 'relink_required';
  await page.locator('#refresh').click();
  await expect(page.locator('#chat-state-notice')).toContainText('重新扫码绑定');
  await expect(page.locator('.message-bubble')).toHaveCount(3);
  model.accounts[0].state = 'ready';
  model.historyError = true;
  await page.locator('#refresh').click();
  await expect(page.locator('#connection-notice')).toBeVisible();
  await expect(page.locator('.message-bubble')).toHaveCount(3);
  await page.locator('#message-input').fill('断线期间的草稿');
  await expect(page.locator('#send-button')).toBeDisabled();
  model.historyError = false;
  await page.locator('#refresh').click();
  await expect(page.locator('#connection-notice')).toBeHidden();
  await expect(page.locator('#send-button')).toBeEnabled();
  model.expired = true;
  await page.locator('#refresh').click();
  await expect(page.locator('#connect-view')).toBeVisible();
  await expect(page.locator('#connect-error')).toContainText('API Token');
  await expect(page.locator('.message-bubble')).toHaveCount(0);
  await expect(page.locator('#message-input')).toHaveValue('');
});

test('the actual unlinked Rust server renders an honest empty account list', async ({ page }) => {
  await page.goto('/');
  await connect(page);
  await expect(page.locator('#account-count')).toHaveText('0');
  await expect(page.locator('#account-list')).toContainText('尚未绑定微信账户');
  await expect(page.locator('.account-row')).toHaveCount(0);
  await page.getByRole('button', { name: '断开连接' }).click();
  await expect(page.locator('#connect-view')).toBeVisible();
});

test('Auth Mini restores the session without a token field, including refresh and logout', async ({ page }) => {
  await fixture(page, { authMini: true });
  await expect(page.locator('#workspace')).toBeVisible();
  await expect(page.getByLabel('API Token')).toBeHidden();
  await open(page);
  await page.reload();
  await expect(page.locator('#workspace')).toBeVisible();
  await expect(page.locator('#chat-pane')).toBeVisible();
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
  let loggedOut = false;
  await page.route('**/logout?**', async (route) => {
    loggedOut = true;
    await route.fulfill({ contentType: 'text/html', body: '<script>location.replace("/?signed_out=1")</script>' });
  });
  await page.getByRole('button', { name: '退出登录' }).click();
  await expect(page.getByRole('button', { name: '使用 Auth Mini 登录' })).toBeVisible();
  await expect(page.locator('#workspace')).toBeHidden();
  expect(loggedOut).toBe(true);
});

test('Auth Mini cookie sends carry the browser origin required by CSRF protection', async ({ page, baseURL }) => {
  // Inspect real HTTP headers: WebKit's intercepted requests omit Cookie metadata.
  // UI assets still come from the actual Rust binary under test.
  let origin;
  let received;
  const messages = [message('incoming', '浏览器来源测试')];
  const server = createServer(async (request, response) => {
    const json = (body, status = 200) => {
      response.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
      response.end(JSON.stringify(body));
    };
    try {
      if (request.url === '/auth/config') return json({ mode: 'auth_mini' });
      if (request.url === '/wechat/accounts') return json({ accounts: [{ id: A, display_name: '微信账户 · 1a7c92', state: 'ready', monitor_running: true }] });
      if (request.url === `/wechat/accounts/${A}/messages`) {
        if (request.method === 'GET') return json({ account_id: A, messages, limit: 200 });
        const chunks = [];
        for await (const chunk of request) chunks.push(chunk);
        const body = JSON.parse(Buffer.concat(chunks).toString());
        received = { origin: request.headers.origin, cookie: request.headers.cookie, text: body.text };
        if (received.origin !== origin) return json({ error: { code: 'invalid_origin' } }, 403);
        if (received.cookie !== 'amg_session=browser-test-session') return json({ error: { code: 'login_required' } }, 401);
        const sent = message('outgoing', body.text, 'outgoing');
        messages.push(sent);
        return json({ message: sent, history_saved: true });
      }
      const asset = await fetch(baseURL + request.url);
      const headers = {};
      for (const name of ['content-type', 'content-security-policy', 'referrer-policy', 'cache-control']) {
        if (asset.headers.has(name)) headers[name] = asset.headers.get(name);
      }
      if (request.url === '/') headers['set-cookie'] = 'amg_session=browser-test-session; Path=/; HttpOnly; SameSite=Lax';
      response.writeHead(asset.status, headers);
      response.end(Buffer.from(await asset.arrayBuffer()));
    } catch {
      response.writeHead(500);
      response.end();
    }
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
  try {
    await page.goto(origin);
    await expect(page.locator('#workspace')).toBeVisible();
    await open(page);
    await page.locator('#message-input').fill('验证浏览器来源');
    await page.locator('#send-button').click();
    await expect(page.locator('#send-error')).toBeHidden();
    await expect(page.locator('#message-input')).toHaveValue('');
    expect(received).toEqual({ origin, cookie: 'amg_session=browser-test-session', text: '验证浏览器来源' });
  } finally {
    await page.goto('about:blank');
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test('Auth Mini expiry redirects initially and asks to sign in again during a conversation', async ({ page }) => {
  const model = await fixture(page, { authMini: true });
  await expect(page.locator('#workspace')).toBeVisible();
  await open(page);
  model.expired = true;
  await page.locator('#refresh').click();
  await expect(page.locator('#connect-error')).toContainText('登录已过期');
  await expect(page.getByRole('button', { name: '使用 Auth Mini 登录' })).toBeVisible();
  await expect(page.locator('.message-bubble')).toHaveCount(0);
  await page.route('**/login?**', (route) => route.fulfill({ contentType: 'text/html', body: '<h1>Auth Mini login fixture</h1>' }));
  await page.getByRole('button', { name: '使用 Auth Mini 登录' }).click();
  await expect(page).toHaveURL(/\/login\?return_to=%2F$/);
  await page.goto('/');
  await expect(page).toHaveURL(/\/login\?return_to=%2F$/);
});

test('Auth Mini denial and outage never fall back to an API Token prompt', async ({ page }) => {
  const model = await fixture(page, { authMini: true, denied: true });
  await expect(page.locator('#connect-error')).toContainText('没有访问权限');
  await expect(page.getByLabel('API Token')).toBeHidden();
  await expect(page.locator('#workspace')).toBeHidden();
  model.denied = false;
  model.authUnavailable = true;
  await page.getByRole('button', { name: '重新检查登录' }).click();
  await expect(page.locator('#connect-error')).toContainText('登录服务暂时不可用');
  await expect(page.getByLabel('API Token')).toBeHidden();
  model.authUnavailable = false;
  await page.getByRole('button', { name: '重新检查登录' }).click();
  await expect(page.locator('#workspace')).toBeVisible();
});
