// パネル追従の直接計測。
//
// CAMetalLayer(displaySyncEnabled=false = wgpu Immediate 相当)へ任意レートでpresentし、
// MTLDrawable.presentedTime(= 実際に画面に出た時刻)の間隔を測る。
//  - パネルが120Hz固定なら presentedTime は 8.333ms 格子に量子化される
//    (55.46Hz狙い → 16.67ms と 25.0ms の混在)
//  - パネルがVRRで追従するなら presentedTime 間隔はターゲット周期そのものになる
//
// CADisplayLink と CGDisplayModeGetRefreshRate はProMotionの実レートを返さない
// (常に120Hzを報告する)ので、判定にはこのpresentedTime計測を使うこと。
//
// usage:
//   swiftc -O present_probe.swift -o present_probe
//   ./present_probe [target_hz] [seconds] [--vsync]     # target_hz=0 で全力present
//   PROBE_DUMP=raw.txt ./present_probe 55.46 12 --vsync # 生のpresentedTimeを出力
//   python3 analyze.py raw.txt                          # 格子周期を推定(numpy必要)

import AppKit
import Metal
import QuartzCore

let targetHz = CommandLine.arguments.count > 1 ? Double(CommandLine.arguments[1]) ?? 55.46 : 55.46
let seconds = CommandLine.arguments.count > 2 ? Double(CommandLine.arguments[2]) ?? 12.0 : 12.0
// --vsync: displaySyncEnabled=true。presentがパネルのリフレッシュに同期するので
// presentedTime間隔 = パネルの実周期(格子)が読める
let useVsync = CommandLine.arguments.contains("--vsync")
let VSYNC = 1.0 / 120.0 * 1000.0  // 120Hzパネルの1フレーム = 8.3333ms

final class Stats: @unchecked Sendable {
    let lock = NSLock()
    var presented: [Double] = []   // presentedTime (ms)
    var submitted: [Double] = []   // present呼び出し時刻の間隔 (ms)
    func addPresented(_ t: Double) { lock.lock(); presented.append(t); lock.unlock() }
    func addSubmitted(_ dt: Double) { lock.lock(); submitted.append(dt); lock.unlock() }
    func drain() -> ([Double], [Double]) {
        lock.lock(); defer { presented.removeAll(); submitted.removeAll(); lock.unlock() }
        return (presented, submitted)
    }
}

func summarize(_ xs: [Double]) -> (Double, Double) {
    guard !xs.isEmpty else { return (0, 0) }
    let n = Double(xs.count)
    let mean = xs.reduce(0, +) / n
    let v = xs.map { ($0 - mean) * ($0 - mean) }.reduce(0, +) / n
    return (mean, v.squareRoot())
}

let stats = Stats()
let app = NSApplication.shared
app.setActivationPolicy(.regular)

guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else {
    fatalError("no metal device")
}
let screen = NSScreen.main!
let window = NSWindow(contentRect: screen.frame, styleMask: [.borderless],
                      backing: .buffered, defer: false, screen: screen)
window.level = .mainMenu + 1
window.isOpaque = true
window.backgroundColor = .black
let view = NSView(frame: screen.frame)
view.wantsLayer = true
let layer = CAMetalLayer()
layer.device = device
layer.pixelFormat = .bgra8Unorm
layer.framebufferOnly = true
layer.displaySyncEnabled = useVsync     // false = wgpu PresentMode::Immediate 相当
layer.maximumDrawableCount = 3
layer.frame = view.bounds
layer.drawableSize = CGSize(width: screen.frame.width * screen.backingScaleFactor,
                            height: screen.frame.height * screen.backingScaleFactor)
view.layer = layer
window.contentView = view
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)

FileHandle.standardError.write(
    "probe2: target \(targetHz > 0 ? "\(targetHz)Hz" : "uncapped"), displaySyncEnabled=\(useVsync), fullscreen \(Int(screen.frame.width))x\(Int(screen.frame.height))\n"
        .data(using: .utf8)!)

let start = CACurrentMediaTime()

Thread.detachNewThread {
    let period = 1.0 / targetHz
    var next = CACurrentMediaTime()
    var lastSubmit: Double = 0
    var hue = 0.0
    while true {
        if targetHz > 0 {
            next += period
            let wait = next - CACurrentMediaTime()
            if wait > 0 { Thread.sleep(forTimeInterval: wait) } else { next = CACurrentMediaTime() }
        }

        guard let drawable = layer.nextDrawable() else { continue }
        let pass = MTLRenderPassDescriptor()
        hue = (hue + 0.02).truncatingRemainder(dividingBy: 1.0)
        pass.colorAttachments[0].texture = drawable.texture
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].storeAction = .store
        pass.colorAttachments[0].clearColor = MTLClearColor(red: hue, green: 0.3, blue: 1.0 - hue, alpha: 1)
        let cb = queue.makeCommandBuffer()!
        cb.makeRenderCommandEncoder(descriptor: pass)!.endEncoding()
        drawable.addPresentedHandler { d in
            if d.presentedTime > 0 { stats.addPresented(d.presentedTime * 1000.0) }
        }
        cb.present(drawable)
        cb.commit()

        let now = CACurrentMediaTime() * 1000.0
        if lastSubmit != 0 { stats.addSubmitted(now - lastSubmit) }
        lastSubmit = now
    }
}

Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
    let (pres, sub) = stats.drain()
    var deltas: [Double] = []
    for i in 1..<max(pres.count, 1) { deltas.append(pres[i] - pres[i - 1]) }
    let (sm, ss) = summarize(sub)
    let (pm, ps) = summarize(deltas)
    // 120Hz格子への量子化度合い: 各間隔を8.333msで割った余りの、格子からのズレ
    let offGrid = deltas.map { d -> Double in
        let r = d.truncatingRemainder(dividingBy: VSYNC)
        return min(r, VSYNC - r)
    }
    let (om, _) = summarize(offGrid)
    let ratios = deltas.map { ($0 / VSYNC).rounded() }
    var hist: [Int: Int] = [:]
    for r in ratios { hist[Int(r), default: 0] += 1 }
    let histStr = hist.sorted { $0.key < $1.key }
        .map { "\($0.key)vsync:\($0.value)" }.joined(separator: " ")
    FileHandle.standardError.write(String(
        format: "probe2: submit %.2fms σ%.2f | presented %.2fms σ%.2f → %.2fHz | off-grid %.2fms | %@\n",
        sm, ss, pm, ps, pm > 0 ? 1000.0 / pm : 0, om, histStr).data(using: .utf8)!)
    if let path = ProcessInfo.processInfo.environment["PROBE_DUMP"] {
        let text = pres.map { String(format: "%.6f", $0) }.joined(separator: "\n") + "\n"
        if let fh = FileHandle(forWritingAtPath: path) {
            fh.seekToEndOfFile(); fh.write(text.data(using: .utf8)!); fh.closeFile()
        } else {
            FileManager.default.createFile(atPath: path, contents: text.data(using: .utf8)!)
        }
    }
    if CACurrentMediaTime() - start > seconds { exit(0) }
}
app.run()
