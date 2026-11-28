// Single source of truth for what a person is allowed to see and do in a project.
// Used everywhere: project tabs, header buttons, right-click menus, keyboard shortcuts,
// command palette/search results, board, memory, artifacts, files, git, and settings.

import { S, collabLink } from './store.js';

export const ALL_PROJECT_TABS = [
  ['', 'Board', 'board'],
  ['files', 'Files', 'folder'],
  ['git', 'Git', 'branch'],
  ['memory', 'Memory', 'memory'],
  ['artifacts', 'Artifacts', 'artifact'],
  ['settings', 'Settings', 'settings'],
];

export const ROLE_LABEL = {
  owner: 'Owner',
  collaborator: 'Collaborator',
  operator: 'Operator',
  viewer: 'Viewer',
};

const TAB_KEY = (id) => 'mr-proj-tab-' + id;
const PREF_KEY = (id) => 'mr-collab-prefs-' + id;

function normPerms(raw) {
  if (!raw) return {};
  if (typeof raw === 'string') {
    try {
      const parsed = JSON.parse(raw);
      return parsed && typeof parsed === 'object' ? parsed : {};
    } catch {
      return {};
    }
  }
  return typeof raw === 'object' ? raw : {};
}

/**
 * Compute effective visibility and permissions for one project (owner or joined).
 * Anything not permitted is hidden (never merely disabled).
 */
export function projectAccess({
  isJoined = false,
  perms = {},
  role = 'owner',
  status = 'ok',
  hasLocalSource = false,
  hasGitSource = false,
  hasOwnThreads = false,
  userId = '',
} = {}) {
  const p = normPerms(perms);
  const revoked = !!isJoined && status === 'revoked';
  const effectiveRole = isJoined ? (role || 'viewer') : 'owner';

  const can = (key) => {
    if (!isJoined) return true;
    if (revoked) return false;
    return !!p[key];
  };

  const canViewSharedThreads = can('view_threads');
  const canViewOthersChats = can('view_others_chats');
  const canCreateThread = can('create_threads');
  const canSeeThreads = !isJoined || (!revoked && (canViewSharedThreads || canCreateThread || !!hasOwnThreads));
  const canUseFiles = !isJoined || (!revoked && !!hasLocalSource && (canViewSharedThreads || canCreateThread));
  const canUseGit = !isJoined || (!revoked && !!hasGitSource && (canViewSharedThreads || canCreateThread || can('merge')));
  const canEditProjectMemory = can('edit_memory');
  const canReadGlobalMemory = can('read_global_memory');
  const canPublishArtifact = can('publish_artifacts');
  const canMerge = can('merge');
  const canViewPresence = !isJoined || (!revoked && can('view_presence') && canViewSharedThreads);
  const canSharePresence = can('share_presence');
  const canSendConflictNote = !isJoined || (!revoked && can('send_conflict_notes') && can('view_presence') && canViewSharedThreads);

  const tabs = ALL_PROJECT_TABS.filter(([key]) => {
    if (revoked) return false;
    if (!isJoined) return true;
    if (key === '') return true;
    if (key === 'files') return canUseFiles;
    if (key === 'git') return canUseGit;
    if (key === 'memory') return true;
    if (key === 'artifacts') return true;
    if (key === 'settings') return true;
    return false;
  });

  const canTab = (tab) => tabs.some(([k]) => k === (tab || ''));

  const canMessageThread = (t) => {
    if (!t) return false;
    if (!isJoined) return true;
    if (revoked) return false;
    if (t.user_id && userId && t.user_id === userId) return true;
    if (t.kind === 'lead') return can('message_lead');
    return can('message_threads');
  };

  const memoryScopes = !isJoined
    ? ['collaborator', 'project', 'global']
    : revoked
      ? []
      : ['collaborator', 'project', ...(canReadGlobalMemory ? ['global'] : [])];

  return {
    isJoined: !!isJoined,
    revoked,
    role: effectiveRole,
    perms: p,
    userId,
    status: isJoined ? status : 'ok',
    hasLocalSource: !isJoined || !!hasLocalSource,
    hasGitSource: !isJoined || !!hasGitSource,
    can,
    tabs,
    canTab,
    canSeeThreads,
    canViewSharedThreads,
    canViewOthersChats,
    canCreateThread,
    canMessageThread,
    canUseFiles,
    canUseGit,
    canEditProjectMemory,
    canReadGlobalMemory,
    canDeleteHostMemory: !isJoined,
    memoryScopes,
    canPublishArtifact,
    canManageArtifactFolders: !isJoined,
    canManageHostArtifacts: !isJoined,
    canMerge,
    canViewPresence,
    canSharePresence,
    canSendConflictNote,
    canEditProjectSettings: !isJoined,
    canManageCollaborators: !isJoined,
    canDeleteProject: !isJoined,
    canLeaveProject: !!isJoined,
    canAskOperator: !!isJoined && !revoked && effectiveRole === 'collaborator',
  };
}

/**
 * Compute access from cached store state (for sidebar menus, command palette, shortcuts).
 */
export function accessFromStore(projectId, extra = {}) {
  const link = collabLink(projectId);
  if (!link) return projectAccess({ isJoined: false });
  const local = S.projects.find((p) => p.id === projectId);
  const hasLocalSource = extra.hasLocalSource ?? !!(local?.folder);
  const hasGitSource = extra.hasGitSource ?? !!(local?.repo_url || local?.folder);
  return projectAccess({
    isJoined: true,
    perms: link.perms,
    role: link.role,
    status: link.status || 'ok',
    hasLocalSource,
    hasGitSource,
    userId: link.user_id || '',
    ...extra,
  });
}

export function rememberProjectTab(projectId, tab) {
  try {
    localStorage.setItem(TAB_KEY(projectId), tab || '');
  } catch { /* private window */ }
}

export function savedProjectTab(projectId, access) {
  try {
    const t = localStorage.getItem(TAB_KEY(projectId));
    if (t !== null && access.canTab(t)) return t;
  } catch { /* private window */ }
  return '';
}

export function collabPrefs(linkId) {
  const defaults = { show_presence: true, notify_notes: true, notify_updates: true };
  try {
    const raw = JSON.parse(localStorage.getItem(PREF_KEY(linkId)) || 'null');
    if (raw && typeof raw === 'object') return { ...defaults, ...raw };
  } catch { /* private window */ }
  return defaults;
}

export function setCollabPref(linkId, patch) {
  const next = { ...collabPrefs(linkId), ...patch };
  try {
    localStorage.setItem(PREF_KEY(linkId), JSON.stringify(next));
  } catch { /* private window */ }
  return next;
}
