// App-private RTP1 batch: magic, closed flag, then (kind, u32 LE length, payload).
const decoder = new TextDecoder('utf-8', {fatal:true});
export function decodeTerminalBatch(buffer) {
  const bytes = buffer instanceof ArrayBuffer ? new Uint8Array(buffer)
    : ArrayBuffer.isView(buffer) ? new Uint8Array(buffer.buffer,buffer.byteOffset,buffer.byteLength) : null;
  if(!bytes || bytes.length<5 || bytes[0]!==82 || bytes[1]!==84 || bytes[2]!==80 || bytes[3]!==49 || bytes[4]>1)throw new Error('无效的终端数据批次');
  const view=new DataView(bytes.buffer,bytes.byteOffset,bytes.byteLength),events=[];
  for(let offset=5;offset<bytes.length;){
    if(bytes.length-offset<5)throw new Error('终端数据记录不完整');
    const kind=bytes[offset],length=view.getUint32(offset+1,true),start=offset+5,end=start+length;
    if(end>bytes.length)throw new Error('终端数据长度超出批次');
    const payload=bytes.subarray(start,end);
    if(kind===0)events.push({type:'data',data:payload});
    else if(kind===1){
      const value=JSON.parse(decoder.decode(payload));
      if(!value || typeof value!=='object' || Array.isArray(value) || typeof value.type!=='string' || value.type==='data')throw new Error('无效的终端状态记录');
      events.push(value);
    }else throw new Error('未知的终端数据记录');
    offset=end;
  }
  return {events,closed:bytes[4]===1};
}
