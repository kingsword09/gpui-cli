import UIKit

// The GPUI FFI entry points are declared in Embedding.h (bridging header):
//   gpui_ios_set_embedded / gpui_ios_register_app / gpui_ios_run_demo
//   gpui_ios_get_window / gpui_ios_view_controller / gpui_ios_layout_view
//   gpui_ios_request_frame / gpui_ios_did_become_active ...

/// UIKit owns this view's frame; GPUI owns only the content rendered inside it.
final class GpuiRootView: UIView {
    private let gpuiWindow: UnsafeMutableRawPointer
    private let contentController: UIViewController

    override init(frame: CGRect) {
        gpui_ios_set_embedded()
        gpui_ios_register_app()
        gpui_ios_run_demo()

        guard let window = gpui_ios_get_window(),
              let controller = gpui_ios_view_controller(window) else {
            fatalError("Could not create the GPUI root view")
        }

        gpuiWindow = window
        contentController = Unmanaged<UIViewController>.fromOpaque(controller).takeUnretainedValue()
        super.init(frame: frame)
        clipsToBounds = true
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is unsupported") }

    func attach(to parent: UIViewController) {
        parent.addChild(contentController)
        addSubview(contentController.view)
        contentController.didMove(toParent: parent)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard bounds.width > 0, bounds.height > 0,
              contentController.view.frame != bounds else { return }
        // Publish the new geometry and its rendered content together, otherwise
        // Core Animation stretches the previous drawable until the next tick.
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        contentController.view.frame = bounds
        gpui_ios_layout_view(gpuiWindow)
        drawFrame()
        CATransaction.commit()
    }

    func drawFrame() { _ = gpui_ios_request_frame(gpuiWindow) }
}

final class RootViewController: UIViewController {
    private var gpuiView: GpuiRootView!
    private var displayLink: CADisplayLink?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground

        gpuiView = GpuiRootView(frame: .zero)
        gpuiView.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(gpuiView)
        gpuiView.attach(to: self)

        NSLayoutConstraint.activate([
            gpuiView.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            gpuiView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            gpuiView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            gpuiView.bottomAnchor.constraint(equalTo: view.keyboardLayoutGuide.topAnchor),
        ])
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        displayLink = CADisplayLink(target: self, selector: #selector(renderFrame))
        displayLink?.add(to: .main, forMode: .common)
    }

    override func viewWillDisappear(_ animated: Bool) {
        displayLink?.invalidate()
        displayLink = nil
        super.viewWillDisappear(animated)
    }

    @objc private func renderFrame() { gpuiView.drawFrame() }
}

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = RootViewController()
        window.makeKeyAndVisible()
        self.window = window
        return true
    }

    func applicationDidBecomeActive(_ application: UIApplication) {
        gpui_ios_did_become_active(nil)
    }

    func applicationWillResignActive(_ application: UIApplication) {
        gpui_ios_will_resign_active(nil)
    }

    func applicationDidEnterBackground(_ application: UIApplication) {
        gpui_ios_did_enter_background(nil)
    }

    func applicationWillTerminate(_ application: UIApplication) {
        gpui_ios_will_terminate(nil)
    }
}
