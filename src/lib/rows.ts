/** True when a double-click landed on a control (play, add, album link), which handles itself. */
export const inControl = (e: MouseEvent) => (e.target as Element | null)?.closest('button, a, details') != null;
