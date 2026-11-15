// The pages the browser checks visit and the instances started by global-setup.js. Pages for the administrator
// use the session global-setup.js saved (`adminState`), so tests never sign in themselves.
export const instances = () => JSON.parse(process.env.GONOF_E2E_INSTANCES || '[]');

// `who` is the account a page needs: anonymous visitors, or the administrator (who is also a member). A `path` that
// depends on the instance's content names one for each (`fixture`, `sample`): the sample content's thread ids, authors
// and words are fixed, the same in every language, so these stay valid.
export const pages = [
  { name: 'home', path: '/', who: 'anonymous' },
  { name: 'forums', path: '/forums', who: 'anonymous' },
  { name: 'forum', path: '/showcase', who: 'anonymous' },
  // A long thread: 78 replies over two pages in the fixture; in the sample, 18 replies with the most nested ones.
  { name: 'thread', path: { fixture: '/thread-103', sample: '/thread-44' }, who: 'anonymous' },
  { name: 'search', path: { fixture: '/search?q=Rust', sample: '/search?q=Docker' }, who: 'anonymous' },
  { name: 'login', path: '/login', who: 'anonymous' },
  { name: 'register', path: '/register', who: 'anonymous' },
  { name: 'forgot', path: '/forgot-password', who: 'anonymous' },
  // An unknown token: the page answers 400 and offers to request a new link.
  { name: 'reset', path: '/reset-password?token=e2e-invalid', who: 'anonymous', status: 400 },
  { name: 'profile', path: '/space/2', who: 'anonymous' },
  { name: 'terms', path: '/terms', who: 'anonymous' },
  { name: 'ranks', path: '/ranks', who: 'anonymous' },
  { name: 'not-found', path: '/no-such-page-e2e', who: 'anonymous', status: 404 },
  // A short thread: six replies in both.
  { name: 'thread-member', path: { fixture: '/thread-104', sample: '/thread-2' }, who: 'admin' },
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

/** The address of `entry` on `instance`. */
export function pathOf(entry, instance) {
  return typeof entry.path === 'string' ? entry.path : entry.path[instance.content];
}
