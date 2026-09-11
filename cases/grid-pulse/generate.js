// Generates a 6x6 grid-pulse NUI Flow case.
// 36 cells, each with derived expression $grid[i] > 0.5 controlling visibility.
const fs = require('fs');
const path = require('path');

const COLS = 6;
const ROWS = 6;
const CELL = 44;
const GAP = 6;
const PADDING = 16;
const GRID_W = COLS * CELL + (COLS - 1) * GAP;
const GRID_H = ROWS * CELL + (ROWS - 1) * GAP;
const SURFACE_W = PADDING * 2 + GRID_W;
const SURFACE_H = PADDING * 2 + GRID_H + 36;

let out = '';
out += `surface root w ${SURFACE_W} h ${SURFACE_H}\n`;
out += `\n`;

// Array input: 36 f32 values
out += `input grid array[36] f32\n`;

// Derived expression per cell: true when value > 0.5
for (let i = 0; i < ROWS * COLS; i++) {
  out += `input cell_${i}_on bool = $grid[${i}] > 0.5\n`;
}
out += `\n`;

// Title
out += `  text title value "GRID PULSE 6x6" x ${PADDING} y 8 w 200 h 20\n`;
out += `\n`;

// Grid container
out += `  panel grid x ${PADDING} y ${PADDING + 28} w ${GRID_W} h ${GRID_H} fill #1a1a22\n`;

// Cells: each cell is visible when its derived expression is true
for (let r = 0; r < ROWS; r++) {
  for (let c = 0; c < COLS; c++) {
    const i = r * COLS + c;
    const x = c * (CELL + GAP);
    const y = r * (CELL + GAP);
    out += `    panel cell_${i} x ${x} y ${y} w ${CELL} h ${CELL} fill #4da6ff visible $cell_${i}_on\n`;
  }
}

out += `\n`;

const outPath = path.join(__dirname, 'grid-pulse.nui');
fs.writeFileSync(outPath, out, 'utf-8');
console.log(`Generated ${outPath} (${out.length} bytes, ${ROWS}x${COLS}=${ROWS*COLS} cells, ${ROWS*COLS} derived exprs)`);
