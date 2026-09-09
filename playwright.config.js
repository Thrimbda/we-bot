import { defineConfig } from '@playwright/test';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

export default defineConfig({
  testDir: './tests/browser',
  fullyParallel: true,
  workers: 2,
  timeout: 30000,
  use: {
    baseURL: 'http://127.0.0.1:3119',
    browserName: process.env.PLAYWRIGHT_BROWSER || 'chromium',
    channel: process.env.PLAYWRIGHT_BROWSER === 'webkit' ? undefined : process.env.PLAYWRIGHT_CHANNEL || undefined,
    viewport: { width: 1440, height: 1000 },
    locale: 'zh-CN',
    timezoneId: 'Asia/Shanghai',
    reducedMotion: 'reduce',
  },
  webServer: {
    command: process.env.WE_BOT_TEST_SERVER_COMMAND || 'cargo run',
    url: 'http://127.0.0.1:3119/health',
    reuseExistingServer: false,
    env: {
      WE_BOT_BIND_ADDR: '127.0.0.1:3119',
      WE_BOT_API_TOKEN: 'we-bot-browser-fixture-token-000000000000',
      WE_BOT_STATE_PATH: join(tmpdir(), `we-bot-ui-fixture-${process.pid}`, 'state.json'),
    },
  },
});
