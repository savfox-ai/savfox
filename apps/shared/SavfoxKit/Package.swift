// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "SavfoxKit",
    platforms: [
        .iOS(.v16),
        .macOS(.v13),
    ],
    products: [
        .library(name: "SavfoxKit", targets: ["SavfoxKit"]),
    ],
    targets: [
        .target(name: "SavfoxKit"),
        .testTarget(name: "SavfoxKitTests", dependencies: ["SavfoxKit"]),
    ]
)
