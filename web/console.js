const $ = (id) => document.getElementById(id);
const state = {
  token: '', authMode: null, connected: false, epoch: 0, accounts: [], selected: null, reachable: false,
  histories: new Map(), drafts: new Map(), pending: new Map(), errors: new Map(),
  rows: new Map(), historyVersion: 0, renderSignature: '', timer: null,
  refreshing: false, controllers: new Set(), composing: false,
};
const POLL_MS = 3000;
const TEST_MESSAGE = '这是一条来自 we-bot 的测试消息。收到后，请在微信里回复。';
const shortTime = new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false });
const fullDate = new Intl.DateTimeFormat('zh-CN', { month: 'long', day: 'numeric' });
const statuses = {
  ready: { label: '可发送', className: 'ready', notice: '' },
  waiting_for_message: { label: '等待微信消息', className: 'warning', notice: '请在微信中给 ClawBot 发一条新消息，更新发送权限后即可继续。草稿会保留，请在恢复后手动发送。' },
  relink_required: { label: '需要重新绑定', className: 'warning', notice: '微信绑定已失效，请通过服务端重新扫码绑定。已有会话仍可查看。' },
  not_linked: { label: '未绑定', className: '', notice: '此账户尚未完成微信绑定。' },
};

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function icon(name) {
  const node = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  node.classList.add('icon');
  node.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS(node.namespaceURI, 'use');
  use.setAttribute('href', `#i-${name}`);
  node.append(use);
  return node;
}

function showText(id, text) {
  const node = $(id);
  node.hidden = !text;
  if (node.textContent !== text) node.textContent = text;
}

function selectedAccount() {
  return state.accounts.find((account) => account.id === state.selected);
}

function statusOf(account) {
  return statuses[account?.state] || statuses.not_linked;
}

class ApiError extends Error {
  constructor(status, code) {
    super(code);
    this.status = status;
    this.code = code;
  }
}

async function request(path, options = {}, token = state.token) {
  const controller = new AbortController();
  state.controllers.add(controller);
  const timeout = setTimeout(() => controller.abort(), 22000);
  try {
    const response = await fetch(path, {
      ...options,
      headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}), ...(options.body ? { 'Content-Type': 'application/json' } : {}) },
      mode: 'same-origin', credentials: state.authMode === 'auth_mini' ? 'same-origin' : 'omit', cache: 'no-store',
      // WebKit sends Origin: null for same-origin POSTs with no-referrer.
      referrerPolicy: 'same-origin', signal: controller.signal,
    });
    const body = await response.json().catch(() => null);
    if (!response.ok) throw new ApiError(response.status, body?.error?.code || 'request_failed');
    if (!body) throw new ApiError(502, 'invalid_response');
    return body;
  } finally {
    clearTimeout(timeout);
    state.controllers.delete(controller);
  }
}

function errorText(error, sending = false) {
  const known = {
    unauthorized: 'API Token 不正确或已失效，请重新连接。',
    login_required: '登录已过期，请重新使用 Auth Mini 登录。',
    access_denied: '当前 Auth Mini 账户没有访问权限，请换用已授权的账户。',
    auth_unavailable: '登录服务暂时不可用，请稍后重新检查。',
    invalid_origin: '发送来源无法验证，请从本站重新打开会话。',
    account_not_found: '这个账户已不再绑定到当前服务器，请刷新账户列表。',
    wechat_not_linked: '请先完成微信绑定，再发送消息。',
    wechat_context_not_ready: '请在微信里给 ClawBot 发一条新消息，更新发送权限后再重试。草稿已保留。',
    wechat_send_blocked: '微信已拒绝本条消息，未发送。请在微信里给 ClawBot 发一条新消息，再回来重试。草稿已保留。',
    wechat_request_rejected: '微信已拒绝本条消息，未发送。草稿已保留，请稍后手动重试。',
    wechat_relink_required: '微信绑定已失效，请重新扫码绑定。',
    rate_limited: '发送过于频繁，请稍后再试。',
    invalid_notification: '消息需包含 1–4,000 个字符。',
    notification_too_large: '消息超过 4,000 个字符，请缩短后再发送。',
    invalid_json: '消息格式无效，请刷新页面后再试。',
  };
  if (known[error.code]) return known[error.code];
  return sending
    ? '未能确认发送结果，消息草稿已保留。请先查看微信，再决定是否重试。'
    : '暂时无法读取服务器数据。请检查连接，或点击刷新重试。';
}

function setReachable(reachable) {
  state.reachable = reachable;
  $('server-indicator').classList.toggle('connected', reachable);
  $('server-indicator').parentElement.title = reachable ? '当前通知服务器可访问' : '当前通知服务器尚未连接';
  updateComposer();
}

function disconnect(message = '') {
  state.epoch += 1;
  clearTimeout(state.timer);
  for (const controller of state.controllers) controller.abort();
  state.token = '';
  state.connected = false;
  state.accounts = [];
  state.selected = null;
  state.histories.clear();
  state.drafts.clear();
  state.pending.clear();
  state.errors.clear();
  state.rows.clear();
  state.historyVersion += 1;
  state.renderSignature = '';
  state.refreshing = false;
  $('account-list').replaceChildren();
  $('messages').replaceChildren();
  $('conversation-announcer').textContent = '';
  $('message-input').value = '';
  $('api-token').value = '';
  $('workspace').hidden = true;
  $('workspace').classList.remove('has-selection');
  $('connect-view').hidden = false;
  $('disconnect').hidden = true;
  $('refresh').disabled = false;
  $('refresh').querySelector('span').textContent = '刷新';
  $('connect-button').disabled = false;
  $('connect-button').querySelector('span').textContent = '连接服务器';
  showText('connect-error', message);
  showText('connection-notice', '');
  setReachable(false);
  if (state.authMode === 'auth_mini') {
    $('connect-title').textContent = '登录通知服务';
    $('connect-description').textContent = '使用你的 Auth Mini 账户，继续查看微信消息。';
    $('auth-login').hidden = false;
    $('auth-login').focus({ preventScroll: true });
  } else {
    $('api-token').focus({ preventScroll: true });
  }
}

function handleReadError(error, epoch) {
  if (epoch !== state.epoch) return;
  if (error.status === 401 || error.code === 'access_denied') {
    disconnect(errorText(error));
    return;
  }
  setReachable(false);
  showText('connection-notice', errorText(error));
  $('sync-label').textContent = '更新中断，将自动重试';
}

function renderAccounts() {
  const list = $('account-list');
  $('account-count').textContent = String(state.accounts.length);
  list.querySelector('.accounts-empty')?.remove();
  const ids = new Set(state.accounts.map((account) => account.id));
  for (const [id, row] of state.rows) {
    if (!ids.has(id)) { row.remove(); state.rows.delete(id); }
  }
  for (const account of state.accounts) {
    let row = state.rows.get(account.id);
    if (!row) {
      row = element('button', 'account-row');
      row.type = 'button';
      row.dataset.accountId = account.id;
      row.append(element('span', 'avatar', '微'));
      row.firstElementChild.setAttribute('aria-hidden', 'true');
      const copy = element('span', 'account-copy');
      copy.append(element('span', 'account-name'), element('span', 'account-status'), element('span', 'account-preview'));
      row.append(copy);
      row.addEventListener('click', () => openAccount(account.id));
      state.rows.set(account.id, row);
      list.append(row);
    }
    row.setAttribute('aria-current', String(state.selected === account.id));
    row.setAttribute('aria-label', `打开 ${account.display_name} 的会话`);
    row.title = account.id;
    row.querySelector('.account-name').textContent = account.display_name;
    const status = statusOf(account);
    const statusNode = row.querySelector('.account-status');
    statusNode.className = `account-status ${status.className}`;
    statusNode.replaceChildren(element('span', 'status-dot'), document.createTextNode(status.label));
    const preview = account.last_message;
    row.querySelector('.account-preview').textContent = preview
      ? `${preview.direction === 'outgoing' ? '服务端：' : ''}${preview.text.replace(/\s+/g, ' ')}`
      : '打开会话，开始收发消息';
  }
  if (!state.accounts.length) {
    const empty = element('div', 'accounts-empty');
    empty.append(element('strong', '', '尚未绑定微信账户'), document.createTextNode('通过服务端完成微信绑定后，账户会自动显示在这里。'));
    list.append(empty);
  }
}

function applyAccounts(accounts) {
  if (!Array.isArray(accounts)) throw new ApiError(502, 'invalid_response');
  state.accounts = accounts;
  const ids = new Set(accounts.map((account) => account.id));
  for (const cache of [state.histories, state.drafts, state.errors]) {
    for (const id of cache.keys()) if (!ids.has(id)) cache.delete(id);
  }
  if (state.selected && !ids.has(state.selected)) closeAccount();
  renderAccounts();
  if (state.selected) renderChatHeader();
  $('welcome-title').textContent = accounts.length ? '让消息有来有回' : '等待你的第一个微信账户';
  $('welcome-description').textContent = accounts.length
    ? '选择左侧的微信账户，发送一条测试消息，也能在这里看到微信里的回复。'
    : '完成微信绑定后，就能在这里发送消息、查看回复。';
}

function renderChatHeader() {
  const account = selectedAccount();
  if (!account) return;
  const status = statusOf(account);
  $('chat-title').textContent = account.display_name;
  $('chat-subtitle').textContent = `${status.label} · ${account.monitor_running ? '回信监听已启动' : '回信监听未启动'}`;
  $('mapping-server').textContent = location.host;
  $('mapping-account').textContent = account.id;
  showText('chat-state-notice', status.notice || (!account.monitor_running ? '回信监听未启动。请检查服务状态，恢复后会自动显示新消息。' : ''));
  showText('send-error', state.errors.get(account.id) || '');
  updateComposer();
}

function closeAccount() {
  state.selected = null;
  state.historyVersion += 1;
  state.renderSignature = '';
  $('chat-pane').hidden = true;
  $('welcome-pane').hidden = false;
  $('workspace').classList.remove('has-selection');
  history.replaceState(null, '', location.pathname + location.search);
  renderAccounts();
}

async function openAccount(id) {
  if (!state.accounts.some((account) => account.id === id)) return;
  state.selected = id;
  state.historyVersion += 1;
  state.renderSignature = '';
  $('workspace').classList.add('has-selection');
  $('welcome-pane').hidden = true;
  $('chat-pane').hidden = false;
  $('account-info').hidden = true;
  $('account-info-button').setAttribute('aria-expanded', 'false');
  $('message-input').value = state.drafts.get(id) || '';
  $('new-messages').hidden = true;
  history.replaceState(null, '', `#account=${encodeURIComponent(id)}`);
  renderAccounts();
  renderChatHeader();
  if (state.histories.has(id)) {
    renderMessages(true);
  } else {
    $('chat-empty').hidden = true;
    $('messages').replaceChildren(element('li', 'loading-placeholder', '正在读取会话…'));
  }
  const epoch = state.epoch;
  try {
    await loadHistory(id, true);
  } catch (error) {
    if (epoch === state.epoch && id === state.selected) {
      if (!state.histories.has(id)) $('messages').replaceChildren(element('li', 'loading-placeholder', '会话读取失败，请点击刷新重试。'));
      handleReadError(error, epoch);
    }
  }
}

async function loadHistory(id, initial = false) {
  const epoch = state.epoch;
  const version = ++state.historyVersion;
  const data = await request(`/wechat/accounts/${encodeURIComponent(id)}/messages`);
  if (epoch !== state.epoch || version !== state.historyVersion || id !== state.selected) return;
  if (data.account_id !== id || !Array.isArray(data.messages)) throw new ApiError(502, 'invalid_response');
  state.histories.set(id, data.messages.filter((message) => message.account_id === id));
  renderMessages(initial);
  updateComposer();
  if (initial && selectedAccount()?.state === 'ready' && !matchMedia('(max-width: 720px)').matches) {
    $('message-input').focus({ preventScroll: true });
  }
}

function validDate(timestamp) {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? null : date;
}

function renderMessages(forceBottom = false) {
  const feed = $('message-feed');
  const saved = state.histories.get(state.selected) || [];
  const pending = state.pending.get(state.selected);
  const messages = pending ? [...saved, pending] : saved;
  const signature = JSON.stringify(messages);
  if (signature === state.renderSignature && !forceBottom) return;
  const hadHistory = state.renderSignature !== '';
  const atBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80;
  const oldTop = feed.scrollTop;
  const anchor = Array.from($('messages').querySelectorAll('.message')).find((node) => node.getBoundingClientRect().bottom > feed.getBoundingClientRect().top);
  const anchorOffset = anchor?.getBoundingClientRect().top;
  const anchorId = anchor?.dataset.messageId;
  const oldIds = new Set(Array.from($('messages').querySelectorAll('.message'), (node) => node.dataset.messageId));
  const fragment = document.createDocumentFragment();
  let lastDay;
  for (const message of messages) {
    const date = validDate(message.created_at_ms);
    const day = date?.toLocaleDateString('zh-CN') || '未知日期';
    if (day !== lastDay) {
      const today = new Date().toLocaleDateString('zh-CN');
      fragment.append(element('li', 'date-divider', day === today ? '今天' : date ? fullDate.format(date) : day));
      lastDay = day;
    }
    const outgoing = message.direction === 'outgoing';
    const row = element('li', `message ${outgoing ? 'outgoing' : 'incoming'}`);
    row.dataset.messageId = message.id;
    const meta = element('div', 'message-meta');
    const time = element('time', '', date ? shortTime.format(date) : '时间未知');
    if (date) { time.dateTime = date.toISOString(); time.title = date.toLocaleString('zh-CN'); }
    meta.append(element('span', '', outgoing ? '服务端' : '微信'), time);
    row.append(meta, element('div', 'message-bubble', message.text));
    if (outgoing) {
      const note = element('div', 'delivery-note');
      if (!message.pending) note.append(icon('check'));
      note.append(document.createTextNode(message.pending ? '正在发送…' : '已提交微信'));
      row.append(note);
    }
    fragment.append(row);
  }
  $('messages').replaceChildren(fragment);
  $('chat-empty').hidden = messages.length > 0;
  state.renderSignature = signature;
  if (hadHistory && !forceBottom) {
    const received = messages.filter((message) => message.direction === 'incoming' && !oldIds.has(message.id));
    if (received.length) {
      $('conversation-announcer').textContent = `收到 ${received.length} 条微信新消息。${received.at(-1).text.slice(0, 160)}`;
    }
  }
  if (forceBottom || atBottom) {
    feed.scrollTop = feed.scrollHeight;
    $('new-messages').hidden = true;
  } else {
    const nextAnchor = Array.from($('messages').querySelectorAll('.message')).find((node) => node.dataset.messageId === anchorId);
    feed.scrollTop = nextAnchor && anchorOffset !== undefined
      ? oldTop + nextAnchor.getBoundingClientRect().top - anchorOffset
      : oldTop;
    if (messages.some((message) => !oldIds.has(message.id))) $('new-messages').hidden = false;
  }
}

function updateComposer() {
  const account = selectedAccount();
  const pending = state.pending.has(state.selected);
  const ready = account?.state === 'ready';
  const count = Array.from($('message-input').value.trim()).length;
  $('message-count').textContent = `${count.toLocaleString('en-US')} / 4,000`;
  $('message-count').classList.toggle('over-limit', count > 4000);
  $('message-input').setAttribute('aria-invalid', String(count > 4000));
  $('message-input').disabled = !account || pending || !ready;
  $('test-message').disabled = !ready || pending;
  $('send-button').disabled = !ready || pending || !state.reachable || !state.histories.has(state.selected) || count < 1 || count > 4000;
  $('send-button').querySelector('span').textContent = pending ? '发送中' : '发送';
  $('message-input').placeholder = ready ? '写一条消息，发送到微信…' : '账户准备就绪后，即可发送消息';
}

async function refresh() {
  if (!state.connected || state.refreshing || document.hidden) return;
  const epoch = state.epoch;
  state.refreshing = true;
  $('refresh').disabled = true;
  $('refresh').querySelector('span').textContent = '刷新中';
  try {
    const data = await request('/wechat/accounts');
    if (epoch !== state.epoch) return;
    applyAccounts(data.accounts);
    if (state.selected && !state.pending.has(state.selected)) await loadHistory(state.selected);
    if (epoch !== state.epoch) return;
    setReachable(true);
    showText('connection-notice', '');
    $('sync-label').textContent = `最近更新 ${shortTime.format(new Date())}`;
  } catch (error) {
    handleReadError(error, epoch);
  } finally {
    if (epoch === state.epoch) {
      state.refreshing = false;
      $('refresh').disabled = false;
      $('refresh').querySelector('span').textContent = '刷新';
      scheduleRefresh();
    }
  }
}

function scheduleRefresh() {
  clearTimeout(state.timer);
  if (state.connected && !document.hidden) state.timer = setTimeout(refresh, POLL_MS);
}

function beginLogin() {
  window.location.assign('/login?return_to=%2F');
}

async function activateWorkspace(accounts) {
  state.connected = true;
  $('connect-view').hidden = true;
  $('workspace').hidden = false;
  $('disconnect').hidden = false;
  $('disconnect').setAttribute('aria-label', state.authMode === 'auth_mini' ? '退出登录' : '断开连接');
  $('disconnect').title = state.authMode === 'auth_mini' ? '退出登录' : '断开连接';
  $('chat-pane').hidden = true;
  $('welcome-pane').hidden = false;
  applyAccounts(accounts);
  setReachable(true);
  const accountId = new URLSearchParams(location.hash.slice(1)).get('account');
  if (accountId && state.accounts.some((account) => account.id === accountId)) await openAccount(accountId);
  else { history.replaceState(null, '', location.pathname + location.search); $('refresh').focus({ preventScroll: true }); }
  scheduleRefresh();
}

async function bootstrapAuth() {
  const epoch = ++state.epoch;
  $('auth-retry').hidden = true;
  $('auth-login').hidden = true;
  showText('connect-error', '');
  $('connect-title').textContent = '正在检查登录状态';
  try {
    const config = await request('/auth/config');
    if (epoch !== state.epoch) return;
    if (!['auth_mini', 'api_token'].includes(config.mode)) throw new ApiError(503, 'auth_unavailable');
    state.authMode = config.mode;
    $('connect-form').hidden = config.mode !== 'api_token';
    $('connect-footnote').hidden = config.mode !== 'api_token';
    if (config.mode === 'api_token') {
      $('connect-title').textContent = '连接通知服务';
      $('connect-description').textContent = '查看微信账户，让每条消息有来有回。';
      return;
    }
    if (new URLSearchParams(location.search).has('signed_out')) {
      history.replaceState(null, '', '/');
      disconnect('你已退出登录。');
      return;
    }
    const data = await request('/wechat/accounts');
    if (epoch !== state.epoch) return;
    await activateWorkspace(data.accounts);
  } catch (error) {
    if (epoch !== state.epoch) return;
    if (state.authMode === 'auth_mini' && error.status === 401) { beginLogin(); return; }
    $('connect-title').textContent = '暂时无法打开账户';
    showText('connect-error', errorText(error));
    $('auth-login').hidden = state.authMode !== 'auth_mini';
    $('auth-retry').hidden = false;
  }
}

$('connect-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const candidate = $('api-token').value.trim();
  if (new TextEncoder().encode(candidate).length < 32 || new TextEncoder().encode(candidate).length > 512) {
    showText('connect-error', 'API Token 应为 32–512 字节，请检查后重试。');
    $('api-token').focus();
    return;
  }
  const epoch = ++state.epoch;
  $('connect-button').disabled = true;
  $('connect-button').querySelector('span').textContent = '正在连接…';
  showText('connect-error', '');
  try {
    const data = await request('/wechat/accounts', {}, candidate);
    if (epoch !== state.epoch) return;
    state.token = candidate;
    $('api-token').value = '';
    await activateWorkspace(data.accounts);
  } catch (error) {
    if (epoch !== state.epoch) return;
    showText('connect-error', errorText(error));
    $('api-token').focus();
  } finally {
    if (epoch === state.epoch) {
      $('connect-button').disabled = false;
      $('connect-button').querySelector('span').textContent = '连接服务器';
    }
  }
});

$('send-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if ($('send-button').disabled) return;
  const id = state.selected;
  const epoch = state.epoch;
  const text = $('message-input').value.trim();
  state.drafts.set(id, $('message-input').value);
  state.errors.delete(id);
  state.pending.set(id, { id: `pending:${Date.now()}`, account_id: id, direction: 'outgoing', text, created_at_ms: Date.now(), pending: true });
  $('conversation-announcer').textContent = '正在发送消息。';
  showText('send-error', '');
  renderMessages(true);
  updateComposer();
  try {
    const result = await request(`/wechat/accounts/${encodeURIComponent(id)}/messages`, { method: 'POST', body: JSON.stringify({ text }) });
    if (epoch !== state.epoch) return;
    if (result.message?.account_id !== id) throw new ApiError(502, 'invalid_response');
    state.pending.delete(id);
    if (state.accounts.some((account) => account.id === id)) {
      state.drafts.delete(id);
      const messages = state.histories.get(id) || [];
      if (!messages.some((message) => message.id === result.message.id)) messages.push(result.message);
      state.histories.set(id, messages.slice(-200));
      state.accounts.find((account) => account.id === id).last_message = result.message;
      if (!result.history_saved) state.errors.set(id, '消息已提交微信，但会话记录未能保存。请检查服务存储。');
      renderAccounts();
    }
    if (state.selected === id) {
      state.historyVersion += 1;
      $('conversation-announcer').textContent = '消息已提交微信。';
      $('message-input').value = '';
      renderMessages();
      showText('send-error', state.errors.get(id) || '');
    }
  } catch (error) {
    if (epoch !== state.epoch) return;
    state.pending.delete(id);
    if (error.status === 401) { disconnect(errorText(error)); return; }
    state.errors.set(id, errorText(error, true));
    if (state.selected === id) { renderMessages(); showText('send-error', state.errors.get(id)); }
  } finally {
    if (epoch === state.epoch) {
      updateComposer();
      if (state.selected === id && !matchMedia('(max-width: 720px)').matches) $('message-input').focus({ preventScroll: true });
      // Re-read the account after a send: upstream rejection can invalidate its sending context.
      refresh();
      scheduleRefresh();
    }
  }
});

$('message-input').addEventListener('input', () => {
  if (state.selected) state.drafts.set(state.selected, $('message-input').value);
  updateComposer();
});
$('message-input').addEventListener('compositionstart', () => { state.composing = true; });
$('message-input').addEventListener('compositionend', () => { state.composing = false; updateComposer(); });
$('message-input').addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing && !state.composing && event.keyCode !== 229) {
    event.preventDefault();
    if (!$('send-button').disabled) $('send-form').requestSubmit();
  }
});
$('test-message').addEventListener('click', () => {
  $('message-input').value = TEST_MESSAGE;
  state.drafts.set(state.selected, TEST_MESSAGE);
  updateComposer();
  $('message-input').focus();
});
$('account-info-button').addEventListener('click', () => {
  $('account-info').hidden = !$('account-info').hidden;
  $('account-info-button').setAttribute('aria-expanded', String(!$('account-info').hidden));
});
$('back').addEventListener('click', () => {
  const id = state.selected;
  closeAccount();
  state.rows.get(id)?.focus({ preventScroll: true });
});
$('refresh').addEventListener('click', refresh);
$('disconnect').addEventListener('click', () => {
  if (state.authMode === 'auth_mini') {
    disconnect();
    window.location.assign('/logout?return_to=%2F%3Fsigned_out%3D1');
  } else disconnect();
});
$('auth-login').addEventListener('click', beginLogin);
$('auth-retry').addEventListener('click', bootstrapAuth);
$('new-messages').addEventListener('click', () => {
  $('message-feed').scrollTop = $('message-feed').scrollHeight;
  $('new-messages').hidden = true;
});
$('message-feed').addEventListener('scroll', () => {
  const feed = $('message-feed');
  if (feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80) $('new-messages').hidden = true;
}, { passive: true });
document.addEventListener('visibilitychange', () => {
  clearTimeout(state.timer);
  if (!document.hidden) refresh();
});
$('server-host').textContent = location.host;
bootstrapAuth();
