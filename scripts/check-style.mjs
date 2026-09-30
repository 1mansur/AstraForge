import fs from 'node:fs';
import path from 'node:path';
const roots=['src','crates/core/src','crates/core/tests','crates/core/examples','src-tauri/src','fuzz/fuzz_targets'];
const failures=[];
function visit(directory){for(const entry of fs.readdirSync(directory,{withFileTypes:true})){const file=path.join(directory,entry.name);if(entry.isDirectory()){visit(file);continue;}if(!/\.(ts|tsx|rs|css)$/.test(file))continue;const source=fs.readFileSync(file,'utf8');const lines=source.replace(/\r\n/g,'\n').split('\n');if(lines.at(-1)==='')lines.pop();lines.forEach((line,index)=>{if(!line.trim())failures.push(`${file}:${index+1}: empty source line`);if(/[ \t]+$/.test(line))failures.push(`${file}:${index+1}: trailing whitespace`);if(/^\s*(\/\/|\/\*|<!--)/.test(line))failures.push(`${file}:${index+1}: source comment`);});}}
roots.forEach(visit);
if(failures.length){process.stderr.write(failures.join('\n')+'\n');process.exitCode=1;}else process.stdout.write('Source formatting constraints passed.\n');
