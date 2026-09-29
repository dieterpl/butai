// The terminal's chrome/model.rs braille_trace: two samples per cell,
// four vertical dots, newest samples on the right. Traffic can be truly silent.
export function brailleTrace(history: readonly number[], cells: number, silent = false): string {
  const count = Math.max(0, Math.floor(cells));
  const tail = history.slice(-count * 2);
  const samples = [...Array(Math.max(0, count * 2 - tail.length)).fill(0), ...tail];
  const maps = [[6, 2, 1, 0], [7, 5, 4, 3]];
  return Array.from({ length: count }, (_, i) => {
    let bits = 0;
    for (let side = 0; side < 2; side++) {
      const value = Number.isFinite(samples[i * 2 + side]) ? samples[i * 2 + side]! : 0;
      const level = silent && value <= 0 ? 0 : Math.max(1, Math.min(4, Math.round(value / 25)));
      for (let dot = 0; dot < level; dot++) bits |= 1 << maps[side]![dot]!;
    }
    return String.fromCodePoint(0x2800 + bits);
  }).join("");
}
