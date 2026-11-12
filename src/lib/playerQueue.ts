import type { PlayerStatus, QueueItem } from './types';

/** What App should do after a player event. */
export type QueueAction = 'none' | 'advance' | 'stop' | 'clear';

/**
 * Decide how a player event affects the queue.
 *
 * `pendingId` is the track a play was requested for but whose `playing`/`error` status has not
 * arrived yet. The player handles commands in order and answers each with one event, so any
 * `ended`/`idle` seen while a play is pending belongs to an earlier track or an earlier Stop.
 */
export function reducePlayerEvent(
  s: PlayerStatus,
  queue: QueueItem[],
  queueIndex: number,
  pendingId: number | null,
): { pendingId: number | null; action: QueueAction } {
  if ((s.state === 'playing' || s.state === 'error') && s.track_id === pendingId) {
    return { pendingId: null, action: 'none' };
  }
  if (s.state === 'ended') {
    // Stale if a play is pending or it is not the track the queue is on.
    if (pendingId !== null || s.track_id !== (queue[queueIndex]?.id ?? null)) return { pendingId, action: 'none' };
    return { pendingId, action: queueIndex + 1 < queue.length ? 'advance' : 'stop' };
  }
  if (s.state === 'idle') {
    // Idle carries no track, so a pending play is the only way to tell it is stale.
    return { pendingId, action: pendingId === null ? 'clear' : 'none' };
  }
  return { pendingId, action: 'none' };
}
