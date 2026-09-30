import Foundation

let fm = FileManager.default

let source = URL(fileURLWithPath: "/tmp/flipper-airbattery.json")

let home = fm.homeDirectoryForCurrentUser
let destDir = home
    .appendingPathComponent("Library")
    .appendingPathComponent("Containers")
    .appendingPathComponent("com.lihaoyun6.AirBattery.widget")
    .appendingPathComponent("Data")
    .appendingPathComponent("Documents")
    .appendingPathComponent("NearcastData")

let dest = destDir.appendingPathComponent("FlipperZero.json")
let temp = destDir.appendingPathComponent("FlipperZero.json.tmp")

let offlineTimeout: TimeInterval = 60

func removeAirBatteryEntry() {
    try? fm.removeItem(at: temp)
    try? fm.removeItem(at: dest)
}

guard fm.fileExists(atPath: source.path) else {
    removeAirBatteryEntry()
    exit(0)
}

guard
    let attrs = try? fm.attributesOfItem(atPath: source.path),
    let modified = attrs[.modificationDate] as? Date
else {
    exit(1)
}

let age = Date().timeIntervalSince(modified)

if age > offlineTimeout {
    removeAirBatteryEntry()
    exit(0)
}

do {
    try fm.createDirectory(
        at: destDir,
        withIntermediateDirectories: true
    )

    let data = try Data(contentsOf: source)

    try data.write(
        to: temp,
        options: .atomic
    )

    if fm.fileExists(atPath: dest.path) {
        try? fm.removeItem(at: dest)
    }

    try fm.moveItem(
        at: temp,
        to: dest
    )

    exit(0)

} catch {
    fputs(
        "FlipperAirBatteryHelper error: \(error)\n",
        stderr
    )
    exit(1)
}
