// The pages the browser checks visit and the instances started by global-setup.js. Pages for the administrator
// use the session global-setup.js saved (`adminState`), so tests never sign in themselves.
export const instances = () => JSON.parse(process.env.GONOF_E2E_INSTANCES || '[]');

// `who` is the account a page needs: anonymous visitors, or the administrator (who is also a member).
export const pages = [
  { name: 'home', path: '/', who: 'anonymous' },
  { name: 'forums', path: '/forums', who: 'anonymous' },
  { name: 'forum', path: '/showcase', who: 'anonymous' },
  { name: 'thread', path: '/thread-103', who: 'anonymous' },
  { name: 'search', path: '/search?q=Rust', who: 'anonymous' },
  { name: 'login', path: '/login', who: 'anonymous' },
  { name: 'register', path: '/register', who: 'anonymous' },
  { name: 'forgot', path: '/forgot-password', who: 'anonymous' },
  // An unknown token: the page answers 400 and offers to request a new link.
  { name: 'reset', path: '/reset-password?token=e2e-invalid', who: 'anonymous', status: 400 },
  { name: 'profile', path: '/space/2', who: 'anonymous' },
  { name: 'terms', path: '/terms', who: 'anonymous' },
  { name: 'ranks', path: '/ranks', who: 'anonymous' },
  { name: 'not-found', path: '/no-such-page-e2e', who: 'anonymous', status: 404 },
  { name: 'thread-member', path: '/thread-104', who: 'admin' },
  { name: 'compose', path: '/new', who: 'admin' },
  { name: 'my', path: '/my', who: 'admin' },
  { name: 'my-account', path: '/my/account', who: 'admin' },
  { name: 'my-profile', path: '/my/profile', who: 'admin' },
  { name: 'notifications', path: '/my/notifications', who: 'admin' },
  { name: 'messages', path: '/my/messages', who: 'admin' },
  { name: 'manage', path: '/manage', who: 'admin' },
  { name: 'manage-settings', path: '/manage/settings', who: 'admin' },
  { name: 'manage-users', path: '/manage/users', who: 'admin' },
];
