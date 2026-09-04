/** Group `eslint --format json` output (on stdin) by area and rule. */
let raw = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  raw += chunk;
});
process.stdin.on('end', () => {
  const files = JSON.parse(raw);
  const counts = new Map();
  const srcDetail = [];

  for (const file of files) {
    const relative = file.filePath.split(/NeuralSwap[\\/]/)[1] ?? file.filePath;
    const area = relative.startsWith('test') ? 'test' : relative.startsWith('scripts') ? 'scripts' : 'src';
    for (const message of file.messages) {
      const key = `${area} | ${message.ruleId ?? 'parse-error'}`;
      counts.set(key, (counts.get(key) ?? 0) + 1);
      if (area === 'src') {
        srcDetail.push(`${relative}:${message.line} ${message.ruleId ?? 'parse'} - ${message.message.slice(0, 120)}`);
      }
    }
  }

  for (const [key, count] of [...counts].sort((a, b) => b[1] - a[1])) {
    console.log(String(count).padStart(4), key);
  }
  if (srcDetail.length > 0) {
    console.log('--- src detail ---');
    for (const line of srcDetail) console.log(line);
  }
});
