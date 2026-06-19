#include <iostream>
#include <vector>

// 冒泡排序函数
void bubbleSort(std::vector<int>& arr) {
    int n = arr.size();
    for (int i = 0; i < n - 1; ++i) {
        // 提前退出标志
        bool swapped = false;
        for (int j = 0; j < n - i - 1; ++j) {
            if (arr[j] > arr[j + 1]) {
                std::swap(arr[j], arr[j + 1]);
                swapped = true;
            }
        }
        // 如果没有发生交换，说明已经有序
        if (!swapped) {
            break;
        }
    }
}

// 打印数组函数
void printArray(const std::vector<int>& arr) {
    for (int val : arr) {
        std::cout << val << " ";
    }
    std::cout << std::endl;
}

int main() {
    std::vector<int> arr = {64, 34, 25, 12, 22, 11, 90};

    std::cout << "排序前: ";
    printArray(arr);

    quickSort(arr);

    std::cout << "排序后: ";
    printArray(arr);

    return 0;
}
