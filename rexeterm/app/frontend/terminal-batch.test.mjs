import test from 'node:test';
import assert from 'node:assert/strict';
import { decodeTerminalBatch } from './terminal-batch.js';

const record=(kind,data)=>{
  const bytes=Buffer.from(data),header=Buffer.alloc(5);header[0]=kind;header.writeUInt32LE(bytes.length,1);
  return Buffer.concat([header,bytes]);
};
const batch=(...records)=>Buffer.concat([Buffer.from([82,84,80,49,1]),...records]);
test('raw output keeps every byte and ordering across metadata without a payload copy',()=>{
  const payload=Buffer.from(Array.from({length:256},(_,i)=>i));
  const packet=batch(record(0,payload),record(1,'{"type":"meta","role":"viewer"}'),record(0,[27,91,51,49,109,0,255]));
  const decoded=decodeTerminalBatch(packet);
  assert.equal(decoded.closed,true);
  assert.deepEqual(Buffer.from(decoded.events[0].data),payload);
  assert.equal(decoded.events[0].data.buffer,packet.buffer);
  assert.equal(decoded.events[0].data.byteOffset,packet.byteOffset+10);
  assert.deepEqual(decoded.events[1],{type:'meta',role:'viewer'});
  assert.deepEqual([...decoded.events[2].data],[27,91,51,49,109,0,255]);
  const buffer=packet.buffer.slice(packet.byteOffset,packet.byteOffset+packet.byteLength);
  assert.deepEqual(decodeTerminalBatch(buffer),decoded);
  assert.deepEqual(decodeTerminalBatch(batch()),{events:[],closed:true});
});
test('malformed batch headers, lengths, kinds and metadata are rejected',()=>{
  for(const packet of [[],[82,84,80,49,2],[82,84,80,50,0],[82,84,80,49,0,0],
    [...batch(),0,255,255,255,255],batch(record(2,[])),batch(record(1,'null')),
    batch(record(1,'[]')),batch(record(1,'{"type":"data","data":[1]}')),batch(record(1,[255])),
    batch(record(1,'{"type":'))])assert.throws(()=>decodeTerminalBatch(Uint8Array.from(packet)));
  assert.throws(()=>decodeTerminalBatch({events:[]}));
});
