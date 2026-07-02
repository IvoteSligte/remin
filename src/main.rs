slint::slint! {
    export component App inherits Window {
        callback key_pressed(string);
        callback key_released(string);

        forward-focus: key-handler;
            key-handler := FocusScope {

            key-pressed(event) => {
                debug(event.text);
                root.key_pressed(event.text);
                accept
            }
            key-released(event) => {
                debug(event.text);
                root.key_released(event.text);
                accept
            }
        }
    }
}

fn main() {
    let app = App::new().unwrap();
    app.on_key_pressed(|s| println!("key pressed: `{}`", s));
    app.on_key_released(|s| println!("key pressed: `{}`", s));
    app.run().unwrap();
}
