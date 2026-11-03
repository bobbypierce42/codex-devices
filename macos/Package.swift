// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "CodexDevices",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "DeviceServices", targets: ["DeviceServices"]),
        .executable(name: "CodexDevices", targets: ["CodexDevicesApp"]),
        .executable(name: "SSHAskpass", targets: ["SSHAskpass"])
    ],
    targets: [
        .target(name: "DeviceServices"),
        .executableTarget(name: "CodexDevicesApp", dependencies: ["DeviceServices"]),
        .executableTarget(name: "SSHAskpass"),
        .testTarget(name: "DeviceServicesTests", dependencies: ["DeviceServices"]),
        .testTarget(name: "AppFlowTests", dependencies: ["DeviceServices"])
    ]
)
