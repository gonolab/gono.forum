import { defineConfig } from '@playwright/test';

// Browser checks against throwaway local instances (tests/e2e/support/instance.js): global setup starts an English
// and a Simplified Chinese instance with SQLite in a temporary directory and Redis off, and removes them afterwards.
// Build first: cargo build -p gono-forum-web --bins && npm run build.
const desktop = { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 };
const mobile = { viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true };

export default defineConfig({
  testDir: './tests/e2e',
  globalSetup: './tests/e2e/support/global-setup.js',
  fullyParallel: true,
  workers: 4,
  timeout: 60000,
  reporter: 'list',
  outputDir: '.local/e2e-results',
  use: { locale: 'en-US', timezoneId: 'UTC', contextOptions: { reducedMotion: 'reduce' } },
  projects: [
    { name: 'desktop-light', use: { ...desktop, colorScheme: 'light' } },
    { name: 'desktop-dark', use: { ...desktop, colorScheme: 'dark' } },
    { name: 'mobile-light', use: { ...mobile, colorScheme: 'light' } },
    { name: 'mobile-dark', use: { ...mobile, colorScheme: 'dark' } },
  ],
});
