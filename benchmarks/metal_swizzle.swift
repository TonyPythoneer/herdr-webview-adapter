import Foundation
import Metal
import Darwin

func emit(_ value: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    print(String(data: data, encoding: .utf8)!)
}
guard let device = MTLCreateSystemDefaultDevice() else {
    emit(["status":"unavailable", "reason":"runner exposes no Metal device"])
    exit(0)
}
let source = """
#include <metal_stdlib>
using namespace metal;
kernel void swizzle(device uchar4 *pixels [[buffer(0)]],
                    constant uint &count [[buffer(1)]],
                    uint i [[thread_position_in_grid]]) {
    if (i < count) { uchar4 p = pixels[i]; pixels[i] = p.bgra; }
}
"""
let setupStart = DispatchTime.now().uptimeNanoseconds
let library = try device.makeLibrary(source: source, options: nil)
guard let fn = library.makeFunction(name: "swizzle"),
      let queue = device.makeCommandQueue() else { fatalError("Metal setup failed") }
let pipeline = try device.makeComputePipelineState(function: fn)
let setupMS = Double(DispatchTime.now().uptimeNanoseconds - setupStart)/1e6
emit(["status":"device_available","device":device.name,"unified_memory":device.hasUnifiedMemory,
      "setup_ms":setupMS])
func cpu(_ ptr: UnsafeMutablePointer<UInt8>, _ count: Int) {
    for i in 0..<count {
        let j=i*4
        let r=ptr[j]; ptr[j]=ptr[j+2]; ptr[j+2]=r
    }
}
for (w,h) in [(640,480),(1024,768),(1920,1080),(3840,2160)] {
    let count=w*h, length=count*4
    let original=(0..<length).map { UInt8(truncatingIfNeeded: $0 &* 31 &+ 7) }
    var expected=original
    expected.withUnsafeMutableBufferPointer { cpu($0.baseAddress!,count) }
    guard let buffer=device.makeBuffer(length:length,options:.storageModeShared) else {fatalError("Metal allocation failed")}
    var cpuBytes=original
    func gpu() {
        guard let cmd=queue.makeCommandBuffer(),let encoder=cmd.makeComputeCommandEncoder() else {fatalError("Metal encode failed")}
        var n=UInt32(count)
        encoder.setComputePipelineState(pipeline)
        encoder.setBuffer(buffer,offset:0,index:0)
        encoder.setBytes(&n,length:MemoryLayout<UInt32>.size,index:1)
        let threads=min(pipeline.maxTotalThreadsPerThreadgroup,256)
        encoder.dispatchThreads(MTLSize(width:count,height:1,depth:1),threadsPerThreadgroup:MTLSize(width:threads,height:1,depth:1))
        encoder.endEncoding(); cmd.commit(); cmd.waitUntilCompleted()
        guard cmd.status == .completed else {fatalError("Metal command failed: \(String(describing:cmd.error))")}
    }
    // Warm pipeline once, then measure CPU-backed image conversion vs
    // Metal including copy-in, dispatch, synchronization and copy-out.
    original.withUnsafeBytes { _ = memcpy(buffer.contents(),$0.baseAddress!,length) }
    gpu()
    for i in 0..<12 {
        for arm in (i%2==0 ? ["cpu","metal"] : ["metal","cpu"]) {
            let t=DispatchTime.now().uptimeNanoseconds
            if arm=="cpu" {
                cpuBytes=original
                cpuBytes.withUnsafeMutableBufferPointer { cpu($0.baseAddress!,count) }
            } else {
                original.withUnsafeBytes { _ = memcpy(buffer.contents(),$0.baseAddress!,length) }
                gpu()
                cpuBytes=Array(UnsafeBufferPointer(start:buffer.contents().assumingMemoryBound(to:UInt8.self),count:length))
            }
            let ms=Double(DispatchTime.now().uptimeNanoseconds-t)/1e6
            guard cpuBytes==expected else {fatalError("RGBA/BGRA mismatch")}
            emit(["status":"verified","round":i,"arm":arm,"width":w,"height":h,"ms":ms])
        }
    }
}
