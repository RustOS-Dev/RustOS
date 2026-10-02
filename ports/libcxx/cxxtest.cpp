// cxxtest: the C++ runtime on RustOS (libc++, libc++abi, libunwind on
// musl): iostreams, containers, strings, exceptions across frames, RTTI,
// static constructors, threads, atomics, chrono and filesystem.
#include <algorithm>
#include <atomic>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <map>
#include <memory>
#include <mutex>
#include <regex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <typeinfo>
#include <vector>

static int passed, failed;
#define CHECK(name, cond)                                                    \
    do {                                                                     \
        if (cond)                                                            \
            passed++;                                                        \
        else {                                                               \
            failed++;                                                        \
            std::cout << "FAIL " << name << " (line " << __LINE__ << ")\n"; \
        }                                                                    \
    } while (0)

struct Base {
    virtual ~Base() = default;
    virtual int f() const { return 1; }
};
struct Derived : Base {
    int f() const override { return 2; }
};

static int constructed;
struct Global {
    Global() { constructed = 42; }
} global;

[[noreturn]] static void deep(int n) {
    if (n == 0)
        throw std::runtime_error("deep");
    deep(n - 1);
    __builtin_unreachable();
}

int main() {
    CHECK("static constructor", constructed == 42);
    std::vector<int> v{5, 3, 9, 1};
    std::sort(v.begin(), v.end());
    CHECK("vector sort", v.front() == 1 && v.back() == 9);
    std::map<std::string, int> m{{"b", 2}, {"a", 1}};
    CHECK("map", m.begin()->first == "a" && m.at("b") == 2);
    std::ostringstream os;
    os << "x=" << 42 << ' ' << 3.5;
    CHECK("ostringstream", os.str() == "x=42 3.5");
    try {
        deep(20);
        CHECK("throw", false);
    } catch (const std::exception &e) {
        CHECK("exception unwound", std::string(e.what()) == "deep");
    }
    try {
        (void)m.at("missing");
    } catch (const std::out_of_range &) {
        CHECK("library exception", true);
    }
    std::unique_ptr<Base> b = std::make_unique<Derived>();
    CHECK("virtual call", b->f() == 2);
    CHECK("dynamic_cast", dynamic_cast<Derived *>(b.get()) != nullptr);
    CHECK("typeid", typeid(*b) == typeid(Derived));
    std::atomic<int> count{0};
    std::mutex mu;
    long total = 0;
    std::vector<std::thread> ts;
    for (int i = 0; i < 4; i++)
        ts.emplace_back([&, i] {
            for (int k = 0; k < 10000; k++) {
                count++;
                std::lock_guard<std::mutex> g(mu);
                total += i;
            }
        });
    for (auto &t : ts)
        t.join();
    CHECK("threads + atomics", count == 40000 && total == 60000);
    auto t0 = std::chrono::steady_clock::now();
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
    CHECK("chrono", std::chrono::steady_clock::now() - t0 >= std::chrono::milliseconds(19));
    std::regex re("([a-z]+)([0-9]+)");
    std::smatch sm;
    std::string s = "abc123";
    CHECK("regex", std::regex_match(s, sm, re) && sm[2] == "123");
    namespace fs = std::filesystem;
    fs::path p = fs::temp_directory_path() / "cxxtest.txt";
    { std::ofstream(p) << "hello\n"; }
    std::ifstream in(p);
    std::string line;
    std::getline(in, line);
    CHECK("fstream", line == "hello" && fs::file_size(p) == 6);
    fs::remove(p);
    CHECK("filesystem", !fs::exists(p));
    std::cout << "cxxtest: " << passed << " passed, " << failed << " failed" << std::endl;
    return failed != 0;
}
