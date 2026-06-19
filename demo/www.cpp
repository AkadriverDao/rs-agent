#include <iostream>
#include <vector>

// 分区函数（快速排序辅助函数）
int partition(std::vector<int>& arr, int low, int high) {
    int pivot = arr[high]; // 选择最后一个元素作为基准
    int i = low - 1;       // i 指向小于基准的最后一个元素

    for (int j = low; j < high; ++j) {
        if (arr[j] < pivot) {
            ++i;
            std::swap(arr[i], arr[j]);
        }
    }
    std::swap(arr[i + 1], arr[high]);
    return i + 1;
}

// 快速排序递归函数
void quickSort(std::vector<int>& arr, int low, int high) {
    if (low < high) {
        int pi = partition(arr, low, high); // 分区索引
        quickSort(arr, low, pi - 1);        // 递归排序左半部分
        quickSort(arr, pi + 1, high);       // 递归排序右半部分
    }
}

// 快速排序封装函数（对外接口）
void quickSort(std::vector<int>& arr) {
    if (!arr.empty()) {
        quickSort(arr, 0, arr.size() - 1);
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

    bubbleSort(arr);

    std::cout << "排序后: ";
    printArray(arr);

    return 0;
}
