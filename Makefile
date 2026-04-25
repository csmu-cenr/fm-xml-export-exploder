BINARY = fm-xml-export-exploder
INSTALL_PATH = /usr/local/bin/$(BINARY)
RELEASE_DIR = target/release
TARGET = target/release/$(BINARY)

.PHONY: release install uninstall clean

.PHONY: clean
clean:
	rm -rf $(RELEASE_DIR)

release:
	cargo build --release

install: release
	cp $(TARGET) $(INSTALL_PATH)

uninstall:
	rm -f $(INSTALL_PATH)