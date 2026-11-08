//! iOS の UIScene ライフサイクルへの対応。
//!
//! ★**iOS 27 の SDK でビルドしたアプリは UIScene を採用しないと起動直後に落ちる。**
//!   UIKit の `_UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption` が
//!   SIGTRAP を投げる(iPhone 15 Pro と iOS 27 シミュレータで確認。iOS 26 の
//!   シミュレータでは落ちない)。winit 0.30 は旧来の AppDelegate 方式でウィンドウを
//!   作るので、それだけでは足りない。
//!
//! winit には手を入れず、外側で辻褄を合わせる:
//!
//! 1. Info.plist の `UIApplicationSceneManifest` で、シーンのデリゲートに
//!    下の `RetroCastXSceneDelegate` を指名する(packaging/ios/Info.plist.in)
//! 2. winit が作ったウィンドウを、繋がったシーンに付ける(シーンの無いウィンドウは
//!    画面に出ない。描画も始まらない)
//!
//! ★**シーンの接続とウィンドウの作成は、どちらが先に来るか決まっていない**
//!   (iOS 27 シミュレータではシーンが先だった)。両方を覚えておき、そろった
//!   ところで付ける。ウィンドウは eframe がアプリを作るときのハンドル(UIView)から
//!   たどる。シーンの無いウィンドウは `UIApplication.windows` に出てこない。

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, AnyProtocol, NSObject};
use objc2::{define_class, msg_send, ClassType, MainThreadOnly};

thread_local! {
    /// 繋がったが、まだウィンドウを付けていないシーン
    static SCENE: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    /// winit が作ったウィンドウの中の UIView
    static VIEW: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSObject に派生の制約は無く、Drop も実装しない
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RetroCastXSceneDelegate"]
    struct SceneDelegate;

    impl SceneDelegate {
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn will_connect(&self, scene: &AnyObject, _session: &AnyObject, _options: &AnyObject) {
            let scene = unsafe { Retained::retain(scene as *const _ as *mut AnyObject) };
            SCENE.with(|s| *s.borrow_mut() = scene);
            try_attach();
        }

        // スクリーンショットの編集・コントロールセンター・アプリ切替などで
        // 前面を離れたら映像を止める(lifecycle.rs)。背後へ回るときも必ず先に来る
        #[unsafe(method(sceneWillResignActive:))]
        fn will_resign_active(&self, _scene: &AnyObject) {
            crate::lifecycle::set_paused(true);
        }

        #[unsafe(method(sceneDidBecomeActive:))]
        fn did_become_active(&self, _scene: &AnyObject) {
            crate::lifecycle::set_paused(false);
        }
    }
);

/// Objective-C のランタイムにクラスを登録する。UIKit は Info.plist に書いた名前で
/// クラスを探すので、`UIApplicationMain`(eframe の起動)より前に呼ぶ
pub fn register() {
    let cls = SceneDelegate::class();
    // ★**UIKit はデリゲートのクラスがプロトコルに準拠しているかを確かめる。**
    //   メソッドがあるだけでは足りず、"delegateClass must conform to
    //   UISceneDelegate protocol" で落ちる。objc2-ui-kit を引かずに済むよう、
    //   ランタイムに準拠だけを足す
    if let Some(proto) = AnyProtocol::get(c"UIWindowSceneDelegate") {
        unsafe { objc2::ffi::class_addProtocol(cls as *const _ as *mut _, proto) };
    }
}

/// eframe がアプリを作るときに呼ぶ(ウィンドウはこの時点で出来ている)
pub fn set_window(cc: &eframe::CreationContext<'_>) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = cc.window_handle() else { return };
    let RawWindowHandle::UiKit(h) = handle.as_raw() else { return };
    let view = unsafe { Retained::retain(h.ui_view.as_ptr() as *mut AnyObject) };
    VIEW.with(|v| *v.borrow_mut() = view);
    try_attach();
}

/// シーンとウィンドウがそろっていたら、ウィンドウをシーンに付けて表に出す
fn try_attach() {
    let Some(view) = VIEW.with(|v| v.borrow().clone()) else { return };
    let Some(scene) = SCENE.with(|s| s.borrow().clone()) else { return };
    unsafe {
        let window: *mut AnyObject = msg_send![&*view, window];
        if window.is_null() {
            return;
        }
        let _: () = msg_send![window, setWindowScene: &*scene];
        let _: () = msg_send![window, makeKeyAndVisible];
    }
    SCENE.with(|s| *s.borrow_mut() = None);
}
