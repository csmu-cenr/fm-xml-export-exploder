BINARY = fm-xml-export-exploder
INSTALL_PATH = /usr/local/bin/$(BINARY)
RELEASE_DIR = target/release
TARGET = target/release/$(BINARY)

.PHONY: clean release install timestamp uninstall

.PHONY: clean
clean:
	rm -rf $(RELEASE_DIR)

release:
	cargo build --release

install: timestamp release
	cp $(TARGET) $(INSTALL_PATH)

timestamp:
	date -u '+%Y%m%d.%H%M%S' > version_timestamp.txt

uninstall:
	rm -f $(INSTALL_PATH)