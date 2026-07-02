use enigo::{
    Direction::{Press, Release},
    Enigo, Key, Keyboard, Settings,
};
use std::sync::{Arc, Mutex};

slint::slint! {
    import { Button } from "std-widgets.slint";
    export component App inherits Window {
        property <string> view: "menu";

        if view == "menu" : Rectangle {
            HorizontalLayout {
                spacing: 10px;
                Button {
                    text: "Start Server";
                    clicked => {
                        debug("Pressed Start Server");
                        root.view = "server";
                    }
                }
                Button {
                    text: "Start Client";
                    clicked => {
                        debug("Pressed Start Client");
                        root.view = "client";
                    }
                }                
            }
        }

        callback key_pressed(string);
        callback key_released(string);
        forward-focus: scope;

        scope := FocusScope {
            KeyBinding {
                keys: @keys(Escape);
                activated => {
                    debug("Escape Pressed");
                    root.view = "menu";
                }
            }
            key-pressed(event) => {
                if (view == "client" && !event.modifiers.meta && event.text != "") {
                    root.key_pressed(event.text);
                }
                accept
            }
            key-released(event) => {
                if (view == "client" && !event.modifiers.meta && event.text != "") {
                    root.key_released(event.text);
                }                
                accept
            }
        }

        if view == "client" : Rectangle {
            Text { text: "Client"; }
        }

        if view == "server" : Rectangle {
            Text { text: "Server"; }
        }
    }
}

fn main() {
    let app = App::new().unwrap();
    let enigo = Arc::new(Mutex::new(Enigo::new(&Settings::default()).unwrap()));
    let enigo2 = enigo.clone();
    app.on_key_pressed(move |s| {
        let Some(c) = s.chars().next() else {
            return;
        };
        if let Err(err) = enigo.lock().unwrap().key(Key::Unicode(c), Press) {
            eprintln!("Failed to press key '{}'. Error: {}", c, err);
        }
    });
    app.on_key_released(move |s| {
        let Some(c) = s.chars().next() else {
            return;
        };
        if let Err(err) = enigo2.lock().unwrap().key(Key::Unicode(c), Release) {
            eprintln!("Failed to release key '{}'. Error: {}", c, err);
        }
    });
    app.run().unwrap();
}
