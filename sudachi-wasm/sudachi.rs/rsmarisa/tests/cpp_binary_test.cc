#include <marisa.h>
#include <iostream>
#include <cstring>

int main(int argc, char *argv[]) {
    if (argc != 2) {
        std::cerr << "Usage: " << argv[0] << " <output_file>" << std::endl;
        return 1;
    }

    const char *output_file = argv[1];

    marisa::Keyset keyset;

    // Same 15 test words as Rust
    const char *words[] = {
        "a", "app", "apple", "application", "apply",
        "banana", "band", "bank", "can", "cat",
        "dog", "door", "test", "testing", "trie",
    };

    for (int i = 0; i < 15; i++) {
        keyset.push_back(words[i]);
    }

    marisa::Trie trie;
    trie.build(keyset);

    trie.save(output_file);
    std::cout << "Saved to '" << output_file << "': " << trie.io_size() << " bytes" << std::endl;

    return 0;
}
